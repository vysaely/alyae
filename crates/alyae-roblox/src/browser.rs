//! What Alyae's Roblox calls and its login window send as their browser, the same for both so a
//! cookie is made and used by one "browser". Two choices (Alyae's Browser setting, passed in
//! ALYAE_BROWSER): Alyae's own fixed identity, desktop Chrome on Windows for every account on
//! Linux and Windows alike (the default), or "system": the user's default browser as it is
//! installed (family and major version), Alyae's identity when that cannot be read.

use std::sync::OnceLock;

/// Chrome stable's major version: chromiumdash.appspot.com (fetch_releases and
/// fetch_milestone_schedule), checked 2026-11-19: 158 rolled out to everyone on 2026-11-18,
/// 159 is out to an early few only.
const CHROME_MAJOR: u32 = 158;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Family {
    Chrome,
    Edge,
    Firefox,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Identity {
    family: Family,
    major: u32,
    windows: bool,
}

/// Alyae's own identity.
const ALYAE: Identity = Identity {
    family: Family::Chrome,
    major: CHROME_MAJOR,
    windows: true,
};

fn identity() -> Identity {
    static ID: OnceLock<Identity> = OnceLock::new();
    *ID.get_or_init(|| {
        if std::env::var("ALYAE_BROWSER").is_ok_and(|v| v == "system") {
            system().unwrap_or(ALYAE)
        } else {
            ALYAE
        }
    })
}

/// The User-Agent the browser sends: Chrome's and Edge's reduced form
/// (chromium.org/updates/ua-reduction: minor versions zeroed, Windows always "Windows NT 10.0;
/// Win64; x64"), Firefox's from developer.mozilla.org/docs/Web/HTTP/Headers/User-Agent/Firefox.
pub fn user_agent() -> String {
    ua(identity())
}

fn ua(id: Identity) -> String {
    let os = if id.windows {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    };
    let m = id.major;
    match id.family {
        Family::Firefox => format!("Mozilla/5.0 ({os}; rv:{m}.0) Gecko/20100101 Firefox/{m}.0"),
        Family::Chrome => format!(
            "Mozilla/5.0 ({os}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{m}.0.0.0 Safari/537.36"
        ),
        Family::Edge => format!(
            "Mozilla/5.0 ({os}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{m}.0.0.0 Safari/537.36 Edg/{m}.0.0.0"
        ),
    }
}

/// The `sec-ch-ua` Chrome of `major` sends: its brand, Chromium and a made-up "Not A Brand"
/// whose spelling, version and place follow from the major version, as Chromium's
/// GetGreasedUserAgentBrandVersion (components/embedder_support/user_agent_utils.cc) makes it.
fn sec_ch_ua(major: u32, brand: &str) -> String {
    const CHARS: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
    const VERSIONS: [&str; 3] = ["8", "99", "24"];
    const ORDERS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let s = major as usize;
    let order = ORDERS[s % 6];
    let mut list = [String::new(), String::new(), String::new()];
    list[order[0]] = format!(
        "\"Not{}A{}Brand\";v=\"{}\"",
        CHARS[s % 11],
        CHARS[(s + 1) % 11],
        VERSIONS[s % 3]
    );
    list[order[1]] = format!("\"Chromium\";v=\"{major}\"");
    list[order[2]] = format!("\"{brand}\";v=\"{major}\"");
    list.join(", ")
}

/// `Accept-Language` from a locale (`de_DE.UTF-8`, Windows' `de-DE`), the bare language at
/// Chrome's default weight (Firefox's is 0.5).
fn accept_language(locale: &str, firefox: bool) -> String {
    let tag = locale
        .split(['.', '@'])
        .next()
        .unwrap_or("")
        .replace('_', "-");
    let tag = if tag.is_empty() || tag == "C" || tag == "POSIX" {
        "en-US".to_string()
    } else {
        tag
    };
    let lang = tag.split('-').next().unwrap_or(&tag);
    if lang == tag {
        return tag;
    }
    let q = if firefox { "0.5" } else { "0.9" };
    format!("{tag},{lang};q={q}")
}

/// The request headers the browser sends besides its User-Agent (names in lower case): client
/// hints from Chrome and Edge, none from Firefox.
pub fn headers() -> Vec<(&'static str, String)> {
    headers_of(identity(), &locale())
}

fn headers_of(id: Identity, locale: &str) -> Vec<(&'static str, String)> {
    let firefox = id.family == Family::Firefox;
    let mut h = Vec::new();
    if !firefox {
        let brand = if id.family == Family::Edge {
            "Microsoft Edge"
        } else {
            "Google Chrome"
        };
        let platform = if id.windows {
            "\"Windows\""
        } else {
            "\"Linux\""
        };
        h.push(("sec-ch-ua", sec_ch_ua(id.major, brand)));
        h.push(("sec-ch-ua-mobile", "?0".into()));
        h.push(("sec-ch-ua-platform", platform.into()));
    }
    h.push(("accept-language", accept_language(locale, firefox)));
    h
}

/// The browser family a default-browser name belongs to (a Linux desktop file such as
/// `firefox.desktop`, a Windows ProgId such as `ChromeHTML`), if Alyae knows it.
fn family_of(name: &str) -> Option<Family> {
    let n = name.to_ascii_lowercase();
    if n.contains("edge") || n.starts_with("msedge") {
        Some(Family::Edge)
    } else if ["firefox", "librewolf", "floorp", "zen", "waterfox"]
        .iter()
        .any(|f| n.contains(f))
    {
        Some(Family::Firefox)
    } else if ["chrome", "chromium", "brave", "vivaldi"]
        .iter()
        .any(|f| n.contains(f))
    {
        Some(Family::Chrome)
    } else {
        None
    }
}

/// The first number in a version line (`Mozilla Firefox 160.0`, `Google Chrome 158.0.7400.1`).
fn major_of(text: &str) -> Option<u32> {
    text.split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse()
        .ok()
        .filter(|m| (50..1000).contains(m))
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut c = std::process::Command::new(program);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = c.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The default browser as installed on Linux: its desktop file names it, its Exec line's program
/// tells the version.
#[cfg(not(windows))]
fn system() -> Option<Identity> {
    let desktop = run("xdg-settings", &["get", "default-web-browser"])
        .or_else(|| run("xdg-mime", &["query", "default", "x-scheme-handler/https"]))?;
    let desktop = desktop.trim();
    let family = family_of(desktop)?;
    let program =
        desktop_exec(desktop).unwrap_or_else(|| desktop.trim_end_matches(".desktop").into());
    let major = major_of(&run(&program, &["--version"])?)?;
    Some(Identity {
        family,
        major,
        windows: false,
    })
}

/// The program a desktop file's Exec line starts.
#[cfg(not(windows))]
fn desktop_exec(desktop: &str) -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = [
        format!("{home}/.local/share/applications"),
        "/usr/local/share/applications".into(),
        "/usr/share/applications".into(),
    ];
    let text = dirs
        .iter()
        .find_map(|d| std::fs::read_to_string(format!("{d}/{desktop}")).ok())?;
    let line = text.lines().find_map(|l| l.strip_prefix("Exec="))?;
    line.split_whitespace().next().map(str::to_string)
}

/// The default browser as installed on Windows: the https ProgId names it, the browser's own
/// registry entry tells the version.
#[cfg(windows)]
fn system() -> Option<Identity> {
    let prog = reg_value(
        r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
        "ProgId",
    )?;
    let family = family_of(&prog)?;
    let version = match family {
        Family::Firefox => reg_value(r"HKLM\SOFTWARE\Mozilla\Mozilla Firefox", "CurrentVersion"),
        Family::Edge => reg_value(r"HKCU\Software\Microsoft\Edge\BLBeacon", "version"),
        Family::Chrome => reg_value(r"HKCU\Software\Google\Chrome\BLBeacon", "version"),
    }?;
    Some(Identity {
        family,
        major: major_of(&version)?,
        windows: true,
    })
}

/// One registry value through `reg query` (the last field of its line).
#[cfg(windows)]
fn reg_value(key: &str, name: &str) -> Option<String> {
    let out = run("reg", &["query", key, "/v", name])?;
    let line = out.lines().find(|l| l.trim_start().starts_with(name))?;
    let value = line.split("REG_SZ").nth(1)?.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(not(windows))]
fn locale() -> String {
    ["LC_ALL", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default()
}

#[cfg(windows)]
fn locale() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85];
    // SAFETY: a buffer of LOCALE_NAME_MAX_LENGTH (85) characters
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    // n counts the closing NUL; 0 is a failure, and then en-US goes
    String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alyae_is_chrome_on_windows() {
        assert_eq!(
            ua(ALYAE),
            format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROME_MAJOR}.0.0.0 Safari/537.36"
            )
        );
        let h = headers_of(ALYAE, "en_US.UTF-8");
        assert_eq!(h[1], ("sec-ch-ua-mobile", "?0".to_string()));
        assert_eq!(h[2], ("sec-ch-ua-platform", "\"Windows\"".to_string()));
    }

    #[test]
    fn firefox_on_linux() {
        let id = Identity {
            family: Family::Firefox,
            major: 160,
            windows: false,
        };
        assert_eq!(
            ua(id),
            "Mozilla/5.0 (X11; Linux x86_64; rv:160.0) Gecko/20100101 Firefox/160.0"
        );
        assert_eq!(
            headers_of(id, "en_US.UTF-8"),
            vec![("accept-language", "en-US,en;q=0.5".to_string())]
        );
    }

    #[test]
    fn edge_names_itself() {
        let id = Identity {
            family: Family::Edge,
            major: 158,
            windows: true,
        };
        assert!(ua(id).ends_with("Safari/537.36 Edg/158.0.0.0"));
        assert!(
            headers_of(id, "C")[0]
                .1
                .contains("\"Microsoft Edge\";v=\"158\"")
        );
    }

    #[test]
    fn default_browser_names() {
        assert_eq!(family_of("firefox.desktop"), Some(Family::Firefox));
        assert_eq!(
            family_of("FirefoxURL-308046B0AF4A39CB"),
            Some(Family::Firefox)
        );
        assert_eq!(family_of("ChromeHTML"), Some(Family::Chrome));
        assert_eq!(family_of("brave-browser.desktop"), Some(Family::Chrome));
        assert_eq!(family_of("MSEdgeHTM"), Some(Family::Edge));
        assert_eq!(family_of("org.gnome.Epiphany.desktop"), None);
        assert_eq!(major_of("Mozilla Firefox 160.0"), Some(160));
        assert_eq!(major_of("Google Chrome 158.0.7400.12 "), Some(158));
        assert_eq!(major_of("160.0 (x64 en-US)"), Some(160));
        assert_eq!(major_of("no version"), None);
    }

    /// The brand lists of published examples, with Chrome's name in place of theirs: Microsoft's
    /// for Edge 120 (learn.microsoft.com, microsoft-edge/web-platform/user-agent-guidance) and
    /// Castle's for Brave 136 (blog.castle.io, how-to-detect-brave-browser-using-http-headers).
    /// Both browsers make the list as Chrome does; only their own name differs.
    #[test]
    fn client_hints_as_chrome_makes_them() {
        assert_eq!(
            sec_ch_ua(120, "Google Chrome"),
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#
        );
        assert_eq!(
            sec_ch_ua(136, "Google Chrome"),
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#
        );
    }

    #[test]
    fn languages() {
        assert_eq!(accept_language("de_DE.UTF-8", false), "de-DE,de;q=0.9");
        assert_eq!(accept_language("en-US", false), "en-US,en;q=0.9");
        assert_eq!(accept_language("C", false), "en-US,en;q=0.9");
        assert_eq!(accept_language("fr", true), "fr");
    }
}
