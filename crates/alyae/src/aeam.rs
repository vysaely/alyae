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
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches(['.', '_']).to_string();
    if s.is_empty() { "unnamed".into() } else { s }
}

struct ReadAccount {
    account: Account,
    label: String,
    bad_secrets: usize,
}

pub struct ImportResult {
    pub added: usize,
    pub skipped: usize,
    pub bad_secrets: Vec<(String, usize)>,
}

/// AEAM's accounts as Alyae's, and the place AEAM last joined.
#[cfg(test)]
pub fn read(dir: &Path) -> Result<(Vec<Account>, Option<String>), String> {
    let (items, place) = read_accounts(dir)?;
    Ok((items.into_iter().map(|r| r.account).collect(), place))
}

fn read_accounts(dir: &Path) -> Result<(Vec<ReadAccount>, Option<String>), String> {
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
    let open = |b: Option<Vec<u8>>, bad: &mut usize| -> Option<String> {
        let b = b?;
        let Some(plain) = vault.open_blob(&b) else {
            *bad += 1;
            return None;
        };
        let Ok(text) = String::from_utf8(plain) else {
            *bad += 1;
            return None;
        };
        (!text.is_empty()).then_some(text)
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
        let (
            id,
            username,
            display,
            user_id,
            cookie,
            group,
            notes,
            tags,
            created,
            last,
            sort,
            password,
            totp,
        ) = row.map_err(|e| e.to_string())?;
        let tags: Vec<String> = serde_json::from_str::<Vec<String>>(&tags.unwrap_or_default())
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.to_lowercase())
            .collect();
        // AEAM's rule: tags decide, then mains.txt
        let main = if tags
            .iter()
            .any(|t| ["main", "165fps", "320fps", "330fps"].contains(&t.as_str()))
        {
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
        let profile = dir.join("instances").join(format!(
            "{}_{}",
            sanitize(&username),
            id.chars().take(8).collect::<String>()
        ));
        if profile.is_dir() {
            fields.insert("profile".into(), profile.to_string_lossy().into_owned());
        }
        let mut bad_secrets = 0;
        let mut secrets = Values::new();
        for (k, v) in [
            ("cookie", open(cookie, &mut bad_secrets)),
            ("password", open(password, &mut bad_secrets)),
            ("totp", open(totp, &mut bad_secrets)),
        ] {
            if let Some(v) = v {
                secrets.insert(k.into(), v);
            }
        }
        let name = if display != username {
            display
        } else {
            String::new()
        };
        let label = if !name.trim().is_empty() {
            name.clone()
        } else if !username.trim().is_empty() {
            username
        } else {
            id.clone()
        };
        out.push(ReadAccount {
            account: Account {
                id,
                game: "roblox".into(),
                name,
                group: group.filter(|g| g != "Default").unwrap_or_default(),
                notes: notes.unwrap_or_default(),
                sort: sort.unwrap_or(0),
                created: created.unwrap_or(0.0),
                last_used: last.unwrap_or(0.0),
                fields,
                secrets,
            },
            label,
            bad_secrets,
        });
    }
    let place = db
        .query_row(
            "SELECT value FROM settings WHERE key = 'SavedPlaceId'",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok();
    Ok((out, place))
}

/// Copies AEAM's accounts into `store`, skipping ones already there.
pub fn import(store: &Store, have: &[Account]) -> Result<ImportResult, String> {
    let dir = dir().ok_or("AEAM's data folder was not found")?;
    import_from(&dir, store, have)
}

pub fn import_from(dir: &Path, store: &Store, have: &[Account]) -> Result<ImportResult, String> {
    let (items, place) = read_accounts(dir)?;
    store.transaction(|| {
        let (mut added, mut skipped) = (0, 0);
        let mut bad_secrets = Vec::new();
        for item in items {
            let a = item.account;
            let dup = have.iter().any(|h| {
                h.id == a.id
                    || (h.game == "roblox"
                        && !a.value("user_id").is_empty()
                        && h.value("user_id") == a.value("user_id"))
            });
            if dup {
                skipped += 1;
                continue;
            }
            store.save(&a)?;
            added += 1;
            if item.bad_secrets > 0 {
                bad_secrets.push((item.label, item.bad_secrets));
            }
        }
        if let Some(p) = place.filter(|_| store.setting("inputs.roblox").is_none()) {
            store.try_set_setting(
                "inputs.roblox",
                &serde_json::json!({ "place": p }).to_string(),
            )?;
        }
        store.try_set_setting("aeam_imported", "1")?;
        Ok(ImportResult {
            added,
            skipped,
            bad_secrets,
        })
    })
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
        assert!(
            accounts
                .iter()
                .all(|a| a.game == "roblox" && !a.value("username").is_empty())
        );
        assert!(
            accounts
                .iter()
                .filter(|a| !a.value("cookie").is_empty())
                .count()
                * 2
                >= accounts.len()
        );
        assert!(accounts.iter().any(|a| a.value("role") == "main"));
        assert!(
            accounts
                .iter()
                .filter(|a| !a.value("profile").is_empty())
                .count()
                * 2
                >= accounts.len()
        );
    }

    struct FakeRow<'a> {
        id: &'a str,
        user: &'a str,
        cookie: Option<&'a [u8]>,
        password: Option<&'a [u8]>,
        totp: Option<&'a [u8]>,
    }

    fn make_fake_aeam(dir: &Path, key: &[u8; 32], rows: &[FakeRow<'_>]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("vault.key"), key).unwrap();
        let db = Connection::open(dir.join("manager.db")).unwrap();
        db.execute_batch(
            "CREATE TABLE accounts (
                 account_id TEXT PRIMARY KEY,
                 username TEXT NOT NULL,
                 display_name TEXT NOT NULL,
                 user_id INTEGER,
                 encrypted_cookie BLOB,
                 group_name TEXT,
                 notes TEXT,
                 tags TEXT,
                 created_at REAL,
                 last_played REAL,
                 sort_order INTEGER,
                 encrypted_password BLOB,
                 encrypted_totp_secret BLOB
             );
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO settings (key, value) VALUES ('SavedPlaceId', '123456');",
        )
        .unwrap();
        for (i, r) in rows.iter().enumerate() {
            db.execute(
                "INSERT INTO accounts (account_id, username, display_name, sort_order, encrypted_cookie, encrypted_password, encrypted_totp_secret)
                 VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![r.id, r.user, i as i64, r.cookie, r.password, r.totp],
            )
            .unwrap();
        }
    }

    #[test]
    fn import_reports_undecryptable_secrets_by_account_name() {
        let base = std::env::temp_dir().join(format!("alyae-aeam-bad-{}", crate::store::new_id()));
        let aeam_dir = base.join("aeam");
        let key = [4u8; 32];
        let vault = Vault::from_key(&key);
        let good_cookie = vault.seal(b"cookie-ok");
        let good_pass = vault.seal(b"pass-ok");
        let wrong_blob = Vault::from_key(&[9u8; 32]).seal(b"wrong-key");
        make_fake_aeam(
            &aeam_dir,
            &key,
            &[
                FakeRow {
                    id: "id1",
                    user: "alice",
                    cookie: Some(&good_cookie),
                    password: None,
                    totp: None,
                },
                FakeRow {
                    id: "id2",
                    user: "bob",
                    cookie: Some(&wrong_blob),
                    password: Some(&good_pass),
                    totp: Some(b"short"),
                },
            ],
        );

        let store = Store::open(&base.join("alyae.db"), Vault::from_key(&[5u8; 32])).unwrap();
        let report = import_from(&aeam_dir, &store, &[]).unwrap();
        assert_eq!(report.added, 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.bad_secrets, vec![("bob".to_string(), 2)]);

        let (accounts, problems) = store.list().unwrap();
        assert!(problems.is_empty());
        assert_eq!(accounts.len(), 2);
        let bob = accounts.iter().find(|a| a.id == "id2").unwrap();
        assert_eq!(bob.value("password"), "pass-ok");
        assert_eq!(bob.value("cookie"), "");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn import_rolls_back_on_failure() {
        let base =
            std::env::temp_dir().join(format!("alyae-aeam-rollback-{}", crate::store::new_id()));
        let aeam_dir = base.join("aeam");
        let key = [4u8; 32];
        let vault = Vault::from_key(&key);
        let c1 = vault.seal(b"c1");
        let c2 = vault.seal(b"c2");
        make_fake_aeam(
            &aeam_dir,
            &key,
            &[
                FakeRow {
                    id: "id1",
                    user: "alice",
                    cookie: Some(&c1),
                    password: None,
                    totp: None,
                },
                FakeRow {
                    id: "id2",
                    user: "bob",
                    cookie: Some(&c2),
                    password: None,
                    totp: None,
                },
            ],
        );

        let db_path = base.join("alyae.db");
        let store = Store::open(&db_path, Vault::from_key(&[5u8; 32])).unwrap();
        {
            let db = Connection::open(&db_path).unwrap();
            db.execute_batch(
                "CREATE TRIGGER fail_bob BEFORE INSERT ON accounts
                 WHEN NEW.id = 'id2'
                 BEGIN
                     SELECT RAISE(ABORT, 'simulated failure');
                 END;",
            )
            .unwrap();
        }

        let err = import_from(&aeam_dir, &store, &[])
            .err()
            .expect("trigger must abort import");
        assert!(err.contains("simulated failure"), "unexpected error: {err}");

        let (accounts, _) = store.list().unwrap();
        assert!(accounts.is_empty(), "partial import was not rolled back");
        assert_eq!(store.setting("aeam_imported"), None);
        assert_eq!(store.setting("inputs.roblox"), None);
        let _ = std::fs::remove_dir_all(base);
    }
}
