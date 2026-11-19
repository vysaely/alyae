//! Roblox's own settings file, `GlobalBasicSettings_13.xml`: frame cap, graphics level, camera
//! sensitivity, volume. Only the named properties are changed; everything else Roblox wrote
//! stays as it is.

use std::path::Path;

/// Keeps the person's own settings file before Alyae first writes into it: only when no Roblox
/// runs (so the file is theirs, not an account's) and no copy is kept already.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn keep_original(live: &Path, original: &Path, nothing_running: bool) {
    if nothing_running && !original.exists() && live.is_file() {
        let _ = std::fs::copy(live, original);
    }
}

/// Puts the person's own settings file back once no Roblox runs any more.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn give_back_original(live: &Path, original: &Path, nothing_running: bool) {
    if nothing_running && original.is_file() && std::fs::copy(original, live).is_ok() {
        let _ = std::fs::remove_file(original);
    }
}

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

/// One property: its XML tag ("int", "token", "float", "bool", or "Vector2" for an X/Y pair:
/// "x,y", or one value for both), name and value.
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
                let (x, y) = axes(&props[i].value);
                for (axis, v) in [("X", x), ("Y", y)] {
                    if let Some(next) = lines
                        .peek()
                        .filter(|l| l.trim_start().starts_with(&format!("<{axis}>")))
                    {
                        let ind = &next[..next.len() - next.trim_start().len()];
                        out += &format!("{ind}<{axis}>{v}</{axis}>\n");
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
                let (x, y) = axes(&p.value);
                format!(
                    "\n\t\t\t<Vector2 name=\"{n}\">\n\t\t\t\t<X>{x}</X>\n\t\t\t\t<Y>{y}</Y>\n\t\t\t</Vector2>",
                    n = p.name,
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

/// A Vector2 value's X and Y: "x,y", or one value for both.
fn axes(v: &str) -> (&str, &str) {
    v.split_once(',').unwrap_or((v, v))
}

/// The "small" window: the least Roblox's PC layout still fits in.
pub const SMALL: (u32, u32) = (800, 600);

/// The account's Window choice: "fullscreen", "windowed" (a normal window) or "small" (a normal
/// window of `SMALL`); anything else (empty) leaves Roblox's own. (On Linux Mocktail sizes the
/// window instead.)
#[cfg_attr(not(windows), allow(dead_code))]
pub fn window(mode: &str) -> Vec<Prop> {
    match mode {
        "fullscreen" => vec![prop("bool", "Fullscreen", true)],
        "windowed" => vec![prop("bool", "Fullscreen", false)],
        "small" => vec![
            prop("bool", "Fullscreen", false),
            prop("bool", "StartMaximized", false),
            prop(
                "Vector2",
                "StartScreenSize",
                format!("{},{}", SMALL.0, SMALL.1),
            ),
        ],
        _ => Vec::new(),
    }
}

/// The Settings form's Window value: None for "keep" (or empty), else a choice `window` takes.
pub fn window_input(v: &str) -> Result<Option<&str>, String> {
    match v {
        "" | "keep" => Ok(None),
        "fullscreen" | "windowed" | "small" => Ok(Some(v)),
        _ => Err(format!("Window: '{v}'")),
    }
}

/// `text` (a settings file, or None: a new one as Roblox would write it) with `props` set.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn with_props(text: Option<&str>, props: &[Prop]) -> String {
    set(text.unwrap_or(NEW_FILE), props)
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
    let tmp = path.with_extension("alyae-tmp");
    std::fs::write(&tmp, new)?;
    std::fs::rename(tmp, path)
}

/// Roblox's menu level (1-10) on its internal scale (1-21), as Roblox itself pairs them
/// (a file at menu level 10 holds 21).
const INTERNAL_LEVEL: [u32; 10] = [1, 3, 5, 7, 9, 11, 14, 16, 18, 21];

/// Graphics level `n` (1-10) chosen in the menu: manual mode, both scales.
pub fn quality(n: u32) -> Vec<Prop> {
    vec![
        prop("token", "GraphicsOptimizationMode", 1),
        prop(
            "int",
            "GraphicsQualityLevel",
            INTERNAL_LEVEL[(n.clamp(1, 10) - 1) as usize],
        ),
        prop("token", "SavedQualityLevel", n.clamp(1, 10)),
        prop("bool", "MaxQualityEnabled", false),
    ]
}

/// One input of the Settings form: its key, the file entry it sets and how its text becomes the
/// value. "keep" (or empty) leaves the entry alone.
struct Setting {
    key: &'static str,
    tag: &'static str,
    name: &'static str,
    value: fn(&str) -> Option<String>,
}

fn on_off(v: &str) -> Option<String> {
    match v {
        "on" => Some("true".into()),
        "off" => Some("false".into()),
        _ => None,
    }
}

/// 0-10 as Roblox's 0-1 volume.
fn tenths(v: &str) -> Option<String> {
    v.parse::<u32>()
        .ok()
        .filter(|n| *n <= 10)
        .map(|n| (n as f32 / 10.0).to_string())
}

fn number(v: &str) -> Option<String> {
    v.replace(',', ".")
        .parse::<f32>()
        .ok()
        .filter(|f| (0.0..=10.0).contains(f))
        .map(|f| f.to_string())
}

fn pick(v: &str, names: &[&str]) -> Option<String> {
    names.iter().position(|n| *n == v).map(|i| i.to_string())
}

const SETTINGS: &[Setting] = &[
    Setting {
        key: "camera_inverted",
        tag: "bool",
        name: "CameraYInverted",
        value: on_off,
    },
    Setting {
        key: "shift_lock",
        tag: "token",
        name: "ControlMode",
        value: |v| on_off(v).map(|b| if b == "true" { "1".into() } else { "0".into() }),
    },
    Setting {
        key: "camera_mode",
        tag: "token",
        name: "ComputerCameraMovementMode",
        value: |v| pick(v, &["Default", "Classic", "Follow", "Orbital"]),
    },
    Setting {
        key: "movement_mode",
        tag: "token",
        name: "ComputerMovementMode",
        value: |v| pick(v, &["Default", "Keyboard", "Click to move"]),
    },
    Setting {
        key: "volume",
        tag: "float",
        name: "MasterVolume",
        value: tenths,
    },
    Setting {
        key: "voice_volume",
        tag: "float",
        name: "VoiceChatVolume",
        value: tenths,
    },
    Setting {
        key: "chat",
        tag: "bool",
        name: "ChatVisible",
        value: on_off,
    },
    Setting {
        key: "stats",
        tag: "bool",
        name: "PerformanceStatsVisible",
        value: on_off,
    },
    Setting {
        key: "reduced_motion",
        tag: "bool",
        name: "ReducedMotion",
        value: on_off,
    },
    Setting {
        key: "translation",
        tag: "bool",
        name: "ChatTranslationEnabled",
        value: on_off,
    },
    Setting {
        key: "transparency",
        tag: "float",
        name: "PreferredTransparency",
        value: |v| number(v).filter(|f| f.parse::<f32>().is_ok_and(|x| x <= 1.0)),
    },
    Setting {
        key: "text_size",
        tag: "token",
        name: "PreferredTextSize",
        value: |v| pick(v, &["", "Medium", "Large", "Larger", "Largest"]),
    },
];

/// The file entries the Settings form asks for, and the FPS cap it sets (0: none), or the first
/// value that is not one Roblox takes.
pub fn from_form(input: impl Fn(&str) -> String) -> Result<(Vec<Prop>, Option<u32>), String> {
    let keep = |v: &str| v.is_empty() || v == "keep";
    let mut props = Vec::new();
    let mut fps = None;
    match input("fps").as_str() {
        v if keep(v) => {}
        "no cap" => {
            fps = Some(0);
            props.push(prop("int", "FramerateCap", 10000));
        }
        v => match v.parse::<u32>() {
            Ok(n @ 1..=1000) => {
                fps = Some(n);
                props.push(prop("int", "FramerateCap", n));
            }
            _ => return Err(format!("FPS limit: '{v}'")),
        },
    }
    match input("quality").as_str() {
        v if keep(v) => {}
        "Automatic" => props.push(prop("token", "GraphicsOptimizationMode", 0)),
        v => match v.parse::<u32>() {
            Ok(n @ 1..=10) => props.extend(quality(n)),
            _ => return Err(format!("Graphics level: '{v}'")),
        },
    }
    match input("sensitivity").as_str() {
        v if keep(v) => {}
        v => match number(v) {
            Some(f) => props.extend(sensitivity(f.parse().unwrap_or(0.2))),
            None => return Err(format!("Camera sensitivity: '{v}' (0 to 10)")),
        },
    }
    for st in SETTINGS {
        let v = input(st.key);
        if keep(&v) {
            continue;
        }
        match (st.value)(&v) {
            Some(value) => props.push(Prop {
                tag: st.tag,
                name: st.name,
                value,
            }),
            None => return Err(format!("{}: '{v}'", st.name)),
        }
    }
    Ok((props, fps))
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
        // level 8 is 16 inside
        props.extend(sensitivity(0.25));
        props.push(prop("int", "FramerateCap", 330));
        let out = set(ROBLOX, &props);
        assert!(out.contains("<int name=\"FramerateCap\">330</int>"));
        // Roblox's tag kept, the missing one added
        assert!(out.contains("<token name=\"SavedQualityLevel\">8</token>"));
        assert!(out.contains("<int name=\"GraphicsQualityLevel\">16</int>"));
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
    fn form_to_entries() {
        let form = |k: &str| match k {
            "fps" => "165".to_string(),
            "quality" => "10".into(),
            "shift_lock" => "on".into(),
            "volume" => "5".into(),
            "camera_mode" => "Classic".into(),
            _ => "keep".into(),
        };
        let (props, fps) = from_form(form).unwrap();
        assert_eq!(fps, Some(165));
        let out = set(ROBLOX, &props);
        assert!(out.contains("<int name=\"FramerateCap\">165</int>"));
        // menu level 10 is 21 inside, as in a real file
        assert!(out.contains("<token name=\"SavedQualityLevel\">10</token>"));
        assert!(out.contains("<int name=\"GraphicsQualityLevel\">21</int>"));
        assert!(out.contains("<token name=\"ControlMode\">1</token>"));
        assert!(out.contains("<float name=\"MasterVolume\">0.5</float>"));
        assert!(out.contains("<token name=\"ComputerCameraMovementMode\">1</token>"));
        assert!(
            from_form(|k| if k == "volume" {
                "11".into()
            } else {
                "keep".into()
            })
            .is_err()
        );
        assert_eq!(from_form(|_| "keep".into()).unwrap().0.len(), 0);
    }

    /// A small window: out of fullscreen, not maximized, its own X and Y; Vector2 pairs that
    /// share one value still get it on both axes.
    #[test]
    fn small_window() {
        let file = "<roblox>\n\t<Item class=\"UserGameSettings\">\n\t\t<Properties>\n\t\t\t<bool name=\"Fullscreen\">true</bool>\n\t\t\t<bool name=\"StartMaximized\">true</bool>\n\t\t\t<Vector2 name=\"StartScreenSize\">\n\t\t\t\t<X>1920</X>\n\t\t\t\t<Y>1080</Y>\n\t\t\t</Vector2>\n\t\t</Properties>\n\t</Item>\n</roblox>\n";
        let out = set(file, &window("small"));
        assert!(out.contains("<bool name=\"Fullscreen\">false</bool>"));
        assert!(out.contains("<bool name=\"StartMaximized\">false</bool>"));
        assert!(out.contains("<X>800</X>") && out.contains("<Y>600</Y>"));
        // a file without the size gets it added
        let added = set(ROBLOX, &window("small"));
        assert!(added.contains("<X>800</X>\n\t\t\t\t<Y>600</Y>"));
        assert!(set(&out, &window("fullscreen")).contains("<bool name=\"Fullscreen\">true</bool>"));
        assert!(window("").is_empty());
        assert_eq!(window_input("keep"), Ok(None));
        assert_eq!(window_input("small"), Ok(Some("small")));
        assert!(window_input("tiny").is_err());
    }

    /// The text a launch would put in place: the same for alts with the same settings (so the
    /// launch needs no wait), a new file when there is none.
    #[test]
    fn launch_text() {
        let props = [prop("int", "FramerateCap", 6)];
        let a = with_props(Some(ROBLOX), &props);
        assert_eq!(with_props(Some(&a), &props), a);
        assert!(with_props(None, &props).contains("UserGameSettings"));
    }

    #[test]
    fn creates_the_file() {
        let dir = std::env::temp_dir().join(format!("alyae-xml-{}", std::process::id()));
        let f = dir.join("appData/GlobalBasicSettings_13.xml");
        apply(&f, &[prop("float", "MasterVolume", 0)]).unwrap();
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(
            t.contains("<float name=\"MasterVolume\">0</float>") && t.contains("UserGameSettings")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The person's own file is kept before the first launch and comes back after the last;
    /// an account's settings never replace the kept copy.
    #[test]
    fn own_settings_come_back() {
        let dir = std::env::temp_dir().join(format!("alyae-orig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (live, original) = (dir.join("live.xml"), dir.join("original.xml"));
        std::fs::write(&live, "theirs").unwrap();
        keep_original(&live, &original, true);
        std::fs::write(&live, "alt account").unwrap();
        // a second launch while one runs keeps the first copy
        keep_original(&live, &original, false);
        give_back_original(&live, &original, false);
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "alt account");
        keep_original(&live, &original, true);
        give_back_original(&live, &original, true);
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "theirs");
        assert!(!original.exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
