//! Alyae's log: one line per event (`2026-11-15 20:51:03 Player1: Join Server: running`) in
//! `<data>/alyae.log`, moved to `alyae.log.1` past 1 MB. Helpers' own error output goes to the same
//! file. Secret values never reach it: changed fields are logged by name only.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static PATH: OnceLock<PathBuf> = OnceLock::new();
const MAX: u64 = 1 << 20;

pub fn init(dir: &std::path::Path) {
    let _ = PATH.set(dir.join("alyae.log"));
}

/// The log, opened for appending (rotated first when full); None before `init` or when the
/// folder cannot be written.
pub fn file() -> Option<File> {
    let path = PATH.get()?;
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    let mut o = OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path).ok()
}

/// UTC time as `YYYY-MM-DD HH:MM:SS`.
fn now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Days since 1970-01-01 as (year, month, day) (Howard Hinnant's civil_from_days).
pub fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

/// Captures `line()` calls on this thread until dropped (tests run concurrently).
#[cfg(test)]
pub struct Capture(());

#[cfg(test)]
impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURE.with(|c| *c.borrow_mut() = None);
    }
}

#[cfg(test)]
pub fn capture() -> Capture {
    CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
    Capture(())
}

#[cfg(test)]
pub fn captured() -> Vec<String> {
    CAPTURE.with(|c| c.borrow().clone().unwrap_or_default())
}

/// Appends one line; `who` is an account name, or empty for Alyae itself.
pub fn line(who: &str, text: &str) {
    let text = text.replace('\n', " ");
    #[cfg(test)]
    CAPTURE.with(|c| {
        if let Some(lines) = c.borrow_mut().as_mut() {
            lines.push(if who.is_empty() {
                text.clone()
            } else {
                format!("{who}: {text}")
            });
        }
    });
    let Some(mut f) = file() else { return };
    let _ = if who.is_empty() {
        writeln!(f, "{} {text}", now())
    } else {
        writeln!(f, "{} {who}: {text}", now())
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_format() {
        let t = super::now();
        assert_eq!(t.len(), 19);
        assert!(t.starts_with("20") && &t[4..5] == "-" && &t[13..14] == ":");
    }
}
