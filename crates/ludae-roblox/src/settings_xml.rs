//! Roblox's own settings file, `GlobalBasicSettings_13.xml`: frame cap, graphics level, camera
//! sensitivity, volume. Only the named properties are changed; everything else Roblox wrote
//! stays as it is.

use std::path::Path;

/// A file Roblox would write on its first start, before any property is set.
const NEW_FILE: &str = r#"<roblox xmlns:xmime="http://www.w3.org/2005/05/xmlmime" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="http://www.roblox.com/roblox.xsd" version="4">
	<External>null</External>
	<External>nil</External>
	<Item class="UserGameSettings" referent="RBX16e4b21d820447a6adf2f8b7d933dc0b">
		<Properties>
			<token name="ControlMode">1</token>
			<bool name="Fullscreen">false</bool>
			<string name="Name">GameSettings</string>
		</Properties>
	</Item>
</roblox>
"#;

/// One property: its XML tag ("int", "token", "float", "bool", or "Vector2" for an X/Y pair
/// set to the same value), name and value.
pub struct Prop {
    pub tag: &'static str,
    pub name: &'static str,
    pub value: String,
}

pub fn prop(tag: &'static str, name: &'static str, value: impl ToString) -> Prop {
    Prop {
        tag,
        name,
        value: value.to_string(),
    }
}

/// `text` with `props` set: an existing property keeps its tag and place, a missing one is added
/// at the top of the properties.
pub fn set(text: &str, props: &[Prop]) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    let mut done = vec![false; props.len()];
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        let found = props
            .iter()
            .position(|p| t.contains(&format!("name=\"{}\"", p.name)));
        match found {
            Some(i) if props[i].tag == "Vector2" => {
                done[i] = true;
                out += line;
                out.push('\n');
                // the <X> and <Y> lines that follow
                for axis in ["X", "Y"] {
                    if let Some(next) = lines
                        .peek()
                        .filter(|l| l.trim_start().starts_with(&format!("<{axis}>")))
                    {
                        let ind = &next[..next.len() - next.trim_start().len()];
                        out += &format!("{ind}<{axis}>{}</{axis}>\n", props[i].value);
                        lines.next();
                    }
                }
            }
            Some(i) => {
                done[i] = true;
                // keep the tag Roblox used (SavedQualityLevel is a token, GraphicsQualityLevel an int)
                let tag = t
                    .strip_prefix('<')
                    .and_then(|r| r.split(' ').next())
                    .unwrap_or(props[i].tag);
                out += &format!(
                    "{indent}<{tag} name=\"{}\">{}</{tag}>\n",
                    props[i].name, props[i].value
                );
            }
            None => {
                out += line;
                out.push('\n');
            }
        }
    }
    let missing: String = props
        .iter()
        .zip(&done)
        .filter(|(_, d)| !**d)
        .map(|(p, _)| {
            if p.tag == "Vector2" {
                format!(
                    "\n\t\t\t<Vector2 name=\"{n}\">\n\t\t\t\t<X>{v}</X>\n\t\t\t\t<Y>{v}</Y>\n\t\t\t</Vector2>",
                    n = p.name,
                    v = p.value
                )
            } else {
                format!("\n\t\t\t<{t} name=\"{n}\">{v}</{t}>", t = p.tag, n = p.name, v = p.value)
            }
        })
        .collect();
    if !missing.is_empty() {
        out = out.replacen("<Properties>", &format!("<Properties>{missing}"), 1);
    }
    out
}

/// Sets `props` in the file at `path`, creating it when Roblox has not written one yet.
pub fn apply(path: &Path, props: &[Prop]) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => NEW_FILE.to_string(),
        Err(e) => return Err(e),
    };
    let new = set(&text, props);
    if new.trim_end() == text.trim_end() {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("ludae-tmp");
    std::fs::write(&tmp, new)?;
    std::fs::rename(tmp, path)
}

/// The graphics level properties for level `n` (1-10), set manually as Roblox's menu does.
pub fn quality(n: u32) -> Vec<Prop> {
    vec![
        prop("token", "GraphicsOptimizationMode", 1),
        prop("int", "GraphicsQualityLevel", n),
        prop("token", "SavedQualityLevel", n),
        prop("bool", "MaxQualityEnabled", false),
    ]
}

/// Camera sensitivity, everywhere Roblox reads it.
pub fn sensitivity(s: f32) -> Vec<Prop> {
    let v = format!("{s:.6}");
    vec![
        prop("float", "MouseSensitivity", &v),
        prop("Vector2", "MouseSensitivityFirstPerson", &v),
        prop("Vector2", "MouseSensitivityThirdPerson", &v),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROBLOX: &str = "<roblox>\n\t<Item class=\"UserGameSettings\">\n\t\t<Properties>\n\t\t\t<int name=\"FramerateCap\">60</int>\n\t\t\t<token name=\"SavedQualityLevel\">3</token>\n\t\t\t<Vector2 name=\"MouseSensitivityFirstPerson\">\n\t\t\t\t<X>0.5</X>\n\t\t\t\t<Y>0.5</Y>\n\t\t\t</Vector2>\n\t\t\t<string name=\"Other\">kept</string>\n\t\t</Properties>\n\t</Item>\n</roblox>\n";

    #[test]
    fn changes_only_what_is_named() {
        let mut props = quality(8);
        props.extend(sensitivity(0.25));
        props.push(prop("int", "FramerateCap", 330));
        let out = set(ROBLOX, &props);
        assert!(out.contains("<int name=\"FramerateCap\">330</int>"));
        // Roblox's tag kept, the missing one added
        assert!(out.contains("<token name=\"SavedQualityLevel\">8</token>"));
        assert!(out.contains("<int name=\"GraphicsQualityLevel\">8</int>"));
        assert!(
            out.contains("<X>0.250000</X>")
                && out.contains("<Y>0.250000</Y>")
                && !out.contains("0.5<")
        );
        assert!(out.contains("<Vector2 name=\"MouseSensitivityThirdPerson\">"));
        assert!(out.contains("<string name=\"Other\">kept</string>"));
        assert_eq!(out.matches("FramerateCap").count(), 1);
        // a second run changes nothing
        assert_eq!(set(&out, &props), out);
    }

    #[test]
    fn creates_the_file() {
        let dir = std::env::temp_dir().join(format!("ludae-xml-{}", std::process::id()));
        let f = dir.join("appData/GlobalBasicSettings_13.xml");
        apply(&f, &[prop("float", "MasterVolume", 0)]).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(
            t.contains("<float name=\"MasterVolume\">0</float>") && t.contains("UserGameSettings")
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
