//! What Alyae's Roblox calls and its login window send as their browser: one fixed identity,
//! desktop Chrome stable on Windows, for every account on Linux and Windows alike, so a cookie
//! is made and used by the same "browser".

/// Chrome stable's major version: chromiumdash.appspot.com (fetch_releases and
/// fetch_milestone_schedule), checked 2026-11-19: 158 rolled out to everyone on 2026-11-18,
/// 159 is out to an early few only.
const CHROME_MAJOR: u32 = 158;

/// Chrome's reduced User-Agent on Windows (chromium.org/updates/ua-reduction): minor versions
/// zeroed, the platform fixed at "Windows NT 10.0; Win64; x64" whatever the Windows version.
pub fn user_agent() -> String {
    format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROME_MAJOR}.0.0.0 Safari/537.36"
    )
}

/// The `sec-ch-ua` Chrome of `major` sends: Google Chrome, Chromium and a made-up "Not A Brand"
/// whose spelling, version and place follow from the major version, as Chromium's
/// GetGreasedUserAgentBrandVersion (components/embedder_support/user_agent_utils.cc) makes it.
fn sec_ch_ua(major: u32) -> String {
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
    list[order[2]] = format!("\"Google Chrome\";v=\"{major}\"");
    list.join(", ")
}

/// `Accept-Language` from a locale (`de_DE.UTF-8`, Windows' `de-DE`), the bare language at
/// Chrome's default weight.
fn accept_language(locale: &str) -> String {
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
    format!("{tag},{lang};q=0.9")
}

/// The request headers Chrome sends besides its User-Agent (names in lower case).
pub fn headers() -> Vec<(&'static str, String)> {
    vec![
        ("sec-ch-ua", sec_ch_ua(CHROME_MAJOR)),
        ("sec-ch-ua-mobile", "?0".into()),
        ("sec-ch-ua-platform", "\"Windows\"".into()),
        ("accept-language", accept_language(&locale())),
    ]
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
    fn chrome_on_windows() {
        assert_eq!(
            user_agent(),
            format!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROME_MAJOR}.0.0.0 Safari/537.36"
            )
        );
        let h = headers();
        assert_eq!(h[1], ("sec-ch-ua-mobile", "?0".to_string()));
        assert_eq!(h[2], ("sec-ch-ua-platform", "\"Windows\"".to_string()));
    }

    /// The brand lists of published examples, with Chrome's name in place of theirs: Microsoft's
    /// for Edge 120 (learn.microsoft.com, microsoft-edge/web-platform/user-agent-guidance) and
    /// Castle's for Brave 136 (blog.castle.io, how-to-detect-brave-browser-using-http-headers).
    /// Both browsers make the list as Chrome does; only their own name differs.
    #[test]
    fn client_hints_as_chrome_makes_them() {
        assert_eq!(
            sec_ch_ua(120),
            r#""Not_A Brand";v="8", "Chromium";v="120", "Google Chrome";v="120""#
        );
        assert_eq!(
            sec_ch_ua(136),
            r#""Chromium";v="136", "Google Chrome";v="136", "Not.A/Brand";v="99""#
        );
    }

    #[test]
    fn languages() {
        assert_eq!(accept_language("de_DE.UTF-8"), "de-DE,de;q=0.9");
        assert_eq!(accept_language("en-US"), "en-US,en;q=0.9");
        assert_eq!(accept_language("C"), "en-US,en;q=0.9");
        assert_eq!(accept_language("fr"), "fr");
    }
}
