//! One-time copy of AEAM's Roblox accounts (`~/.local/share/aeam`). AEAM's files are only read.
//! Each account keeps AEAM's profile folder, so its in-game settings and caches stay.

use crate::store::{Account, Store, Values};
use crate::vault::Vault;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn dir() -> Option<PathBuf> {
    let d = dirs::data_dir()?.join("aeam");
    d.join("manager.db").is_file().then_some(d)
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' })
        .collect();
    let s = s.trim_matches(['.', '_']).to_string();
    if s.is_empty() { "unnamed".into() } else { s }
}

/// AEAM's accounts as Ludae's, and the place AEAM last joined.
pub fn read(dir: &Path) -> Result<(Vec<Account>, Option<String>), String> {
    let key = std::fs::read(dir.join("vault.key")).map_err(|e| format!("AEAM's key: {e}"))?;
    if key.len() != 32 {
        return Err("AEAM's key file is not a key".into());
    }
    let vault = Vault::from_key(&key);
    let db = Connection::open_with_flags(dir.join("manager.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let mains: HashSet<String> = std::fs::read_to_string(dir.join("mains.txt"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_lowercase())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let mut st = db
        .prepare(
            "SELECT account_id, username, display_name, user_id, encrypted_cookie, group_name, notes, tags,
                    created_at, last_played, sort_order, encrypted_password, encrypted_totp_secret
             FROM accounts ORDER BY sort_order",
        )
        .map_err(|e| e.to_string())?;
    let open = |b: Option<Vec<u8>>| -> Option<String> {
        let plain = vault.open_blob(&b?)?;
        String::from_utf8(plain).ok().filter(|s| !s.is_empty())
    };
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<Vec<u8>>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<f64>>(8)?,
                r.get::<_, Option<f64>>(9)?,
                r.get::<_, Option<i64>>(10)?,
                r.get::<_, Option<Vec<u8>>>(11)?,
                r.get::<_, Option<Vec<u8>>>(12)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (id, username, display, user_id, cookie, group, notes, tags, created, last, sort, password, totp) =
            row.map_err(|e| e.to_string())?;
        let tags: Vec<String> = serde_json::from_str::<Vec<String>>(&tags.unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.to_lowercase())
            .collect();
        // AEAM's rule: tags decide, then mains.txt
        let main = if tags.iter().any(|t| ["main", "165fps", "320fps", "330fps"].contains(&t.as_str())) {
            true
        } else if tags.iter().any(|t| t == "alt" || t == "24fps") {
            false
        } else {
            mains.contains(&username.to_lowercase())
        };
        let mut fields = Values::new();
        fields.insert("username".into(), username.clone());
        if let Some(u) = user_id.filter(|u| *u > 0) {
            fields.insert("user_id".into(), u.to_string());
        }
        fields.insert("role".into(), if main { "main" } else { "alt" }.into());
        if let Some(fps) = tags.iter().find_map(|t| t.strip_prefix("fps:")) {
            fields.insert("fps".into(), fps.to_string());
        }
        let profile = dir.join("instances").join(format!("{}_{}", sanitize(&username), id.chars().take(8).collect::<String>()));
        if profile.is_dir() {
            fields.insert("profile".into(), profile.to_string_lossy().into_owned());
        }
        let mut secrets = Values::new();
        for (k, v) in [("cookie", open(cookie)), ("password", open(password)), ("totp", open(totp))] {
            if let Some(v) = v {
                secrets.insert(k.into(), v);
            }
        }
        out.push(Account {
            id,
            game: "roblox".into(),
            name: if display != username { display } else { String::new() },
            group: group.filter(|g| g != "Default").unwrap_or_default(),
            notes: notes.unwrap_or_default(),
            sort: sort.unwrap_or(0),
            created: created.unwrap_or(0.0),
            last_used: last.unwrap_or(0.0),
            fields,
            secrets,
        });
    }
    let place = db
        .query_row("SELECT value FROM settings WHERE key = 'SavedPlaceId'", [], |r| r.get::<_, String>(0))
        .ok();
    Ok((out, place))
}

/// Copies AEAM's accounts into `store`, skipping ones already there. Returns (added, skipped).
pub fn import(store: &Store, have: &[Account]) -> Result<(usize, usize), String> {
    let dir = dir().ok_or("AEAM's data folder was not found")?;
    let (accounts, place) = read(&dir)?;
    let (mut added, mut skipped) = (0, 0);
    for a in accounts {
        let dup = have.iter().any(|h| {
            h.id == a.id || (h.game == "roblox" && !a.value("user_id").is_empty() && h.value("user_id") == a.value("user_id"))
        });
        if dup {
            skipped += 1;
            continue;
        }
        store.save(&a)?;
        added += 1;
    }
    if let Some(p) = place.filter(|_| store.setting("inputs.roblox").is_none()) {
        store.set_setting("inputs.roblox", &serde_json::json!({ "place": p }).to_string());
    }
    store.set_setting("aeam_imported", "1");
    Ok((added, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the owner's real AEAM data when present (read-only); skipped elsewhere.
    #[test]
    fn reads_aeam_when_present() {
        let Some(d) = dir() else { return };
        let (accounts, _) = read(&d).unwrap();
        assert!(!accounts.is_empty());
        assert!(accounts.iter().all(|a| a.game == "roblox" && !a.value("username").is_empty()));
        assert!(accounts.iter().filter(|a| !a.value("cookie").is_empty()).count() * 2 >= accounts.len());
        assert!(accounts.iter().any(|a| a.value("role") == "main"));
        assert!(accounts.iter().filter(|a| !a.value("profile").is_empty()).count() * 2 >= accounts.len());
    }
}
