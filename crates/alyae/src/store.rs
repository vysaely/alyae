//! Accounts in one SQLite file. Plain fields are JSON; secret fields are one sealed JSON blob.

use crate::vault::Vault;
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::path::Path;

pub type Values = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub id: String,
    /// the module id ("roblox", "generic", ...)
    pub game: String,
    pub name: String,
    pub group: String,
    pub notes: String,
    pub sort: i64,
    pub created: f64,
    pub last_used: f64,
    pub fields: Values,
    pub secrets: Values,
}

impl Account {
    pub fn value(&self, key: &str) -> &str {
        self.fields
            .get(key)
            .or_else(|| self.secrets.get(key))
            .map_or("", String::as_str)
    }
}

pub struct Store {
    db: Connection,
    vault: Vault,
}

pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

pub fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("system random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

const SCHEMA_VERSION: u32 = 1;

type Migration = (u32, fn(&Connection) -> Result<(), String>);

/// Ordered schema migrations after version 1; each entry moves from the previous version to `target`.
const MIGRATIONS: &[Migration] = &[];

impl Store {
    pub fn open(db_path: &Path, vault: Vault) -> Result<Self, String> {
        let db = Connection::open(db_path).map_err(|e| e.to_string())?;
        let mut version: i64 = db
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if version > i64::from(SCHEMA_VERSION) || version < 0 {
            return Err(format!(
                "the database is from a newer Alyae (version {version}, this one knows {SCHEMA_VERSION})"
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600));
        }
        db.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| e.to_string())?;
        if version == 0 {
            let has_tables: bool = db
                .query_row(
                    "SELECT count(*) = 2 FROM sqlite_master WHERE type = 'table' AND name IN ('accounts', 'settings')",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if has_tables {
                // Existing pre-versioned database uses the v1 layout.
                db.pragma_update(None, "user_version", 1)
                    .map_err(|e| e.to_string())?;
                version = 1;
            } else {
                db.execute_batch(
                    "CREATE TABLE IF NOT EXISTS accounts (
                         id TEXT PRIMARY KEY, game TEXT NOT NULL, name TEXT NOT NULL,
                         grp TEXT NOT NULL DEFAULT '', notes TEXT NOT NULL DEFAULT '',
                         sort INTEGER NOT NULL DEFAULT 0, created REAL NOT NULL DEFAULT 0,
                         last_used REAL NOT NULL DEFAULT 0, fields TEXT NOT NULL DEFAULT '{}',
                         secrets BLOB);
                     CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
                )
                .map_err(|e| e.to_string())?;
                db.pragma_update(None, "user_version", SCHEMA_VERSION)
                    .map_err(|e| e.to_string())?;
                version = i64::from(SCHEMA_VERSION);
            }
        }
        for &(target, migrate) in MIGRATIONS {
            if version < i64::from(target) {
                let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
                migrate(&db)?;
                db.pragma_update(None, "user_version", target)
                    .map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                version = i64::from(target);
            }
        }
        Ok(Self { db, vault })
    }

    pub fn list(&self) -> Result<(Vec<Account>, Vec<String>), String> {
        let mut st = self
            .db
            .prepare("SELECT id, game, name, grp, notes, sort, created, last_used, fields, secrets FROM accounts ORDER BY sort, name COLLATE NOCASE")
            .map_err(|e| e.to_string())?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    Account {
                        id: r.get(0)?,
                        game: r.get(1)?,
                        name: r.get(2)?,
                        group: r.get(3)?,
                        notes: r.get(4)?,
                        sort: r.get(5)?,
                        created: r.get(6)?,
                        last_used: r.get(7)?,
                        fields: Values::new(),
                        secrets: Values::new(),
                    },
                    r.get::<_, String>(8)?,
                    r.get::<_, Option<Vec<u8>>>(9)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut problems = Vec::new();
        for row in rows {
            let (mut a, fields, blob) = match row {
                Ok(r) => r,
                Err(e) => {
                    problems.push(format!("cannot read an account row: {e}"));
                    continue;
                }
            };
            match serde_json::from_str(&fields) {
                Ok(f) => a.fields = f,
                Err(e) => {
                    problems.push(format!("account {}: damaged fields ({e})", a.id));
                    continue;
                }
            }
            if let Some(b) = blob {
                let Some(plain) = self.vault.open_blob(&b) else {
                    problems.push(format!("account {}: cannot decrypt secrets", a.id));
                    continue;
                };
                match serde_json::from_slice(&plain) {
                    Ok(s) => a.secrets = s,
                    Err(e) => {
                        problems.push(format!("account {}: damaged secrets ({e})", a.id));
                        continue;
                    }
                }
            }
            out.push(a);
        }
        Ok((out, problems))
    }

    pub fn transaction<T>(&self, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let tx = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        let out = f()?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(out)
    }

    pub fn save(&self, a: &Account) -> Result<(), String> {
        let secrets = (!a.secrets.is_empty()).then(|| {
            self.vault.seal(
                serde_json::to_string(&a.secrets)
                    .expect("a string map serializes")
                    .as_bytes(),
            )
        });
        self.db
            .execute(
                "INSERT INTO accounts (id, game, name, grp, notes, sort, created, last_used, fields, secrets)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(id) DO UPDATE SET game=?2, name=?3, grp=?4, notes=?5, sort=?6,
                     created=?7, last_used=?8, fields=?9, secrets=?10",
                params![
                    a.id,
                    a.game,
                    a.name,
                    a.group,
                    a.notes,
                    a.sort,
                    a.created,
                    a.last_used,
                    serde_json::to_string(&a.fields).expect("a string map serializes"),
                    secrets
                ],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        self.db
            .execute("DELETE FROM accounts WHERE id = ?1", [id])
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn setting(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.try_set_setting(key, value);
    }

    pub fn try_set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.db
            .execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=?2",
                [key, value],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("alyae-{tag}-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn user_version(db_path: &Path) -> i64 {
        let db = Connection::open(db_path).unwrap();
        db.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn round_trip() {
        let dir = temp_dir("store");
        let s = Store::open(&dir.join("t.db"), Vault::from_key(&[1; 32])).unwrap();
        let mut a = Account {
            id: new_id(),
            game: "generic".into(),
            name: "me".into(),
            ..Default::default()
        };
        a.fields.insert("login".into(), "me@x".into());
        a.secrets.insert("password".into(), "hunter2".into());
        s.save(&a).unwrap();
        assert_eq!(s.list().unwrap(), (vec![a.clone()], vec![]));
        s.delete(&a.id).unwrap();
        assert_eq!(s.list().unwrap(), (vec![], vec![]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fresh_db_sets_user_version_1() {
        let dir = temp_dir("ver-fresh");
        let db_path = dir.join("t.db");
        let _s = Store::open(&db_path, Vault::from_key(&[1; 32])).unwrap();
        assert_eq!(user_version(&db_path), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn existing_v0_db_with_tables_upgrades_to_1() {
        let dir = temp_dir("ver-v0");
        let db_path = dir.join("t.db");
        {
            let db = Connection::open(&db_path).unwrap();
            db.execute_batch(
                "CREATE TABLE accounts (
                     id TEXT PRIMARY KEY, game TEXT NOT NULL, name TEXT NOT NULL,
                     grp TEXT NOT NULL DEFAULT '', notes TEXT NOT NULL DEFAULT '',
                     sort INTEGER NOT NULL DEFAULT 0, created REAL NOT NULL DEFAULT 0,
                     last_used REAL NOT NULL DEFAULT 0, fields TEXT NOT NULL DEFAULT '{}',
                     secrets BLOB);
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO accounts (id, game, name) VALUES ('a1', 'generic', 'alice');",
            )
            .unwrap();
        }
        assert_eq!(user_version(&db_path), 0);
        let s = Store::open(&db_path, Vault::from_key(&[1; 32])).unwrap();
        assert_eq!(user_version(&db_path), 1);
        let (accounts, problems) = s.list().unwrap();
        assert!(problems.is_empty());
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "a1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn newer_version_is_refused_without_touching_db() {
        let dir = temp_dir("ver-newer");
        let db_path = dir.join("t.db");
        {
            let db = Connection::open(&db_path).unwrap();
            db.pragma_update(None, "user_version", 99).unwrap();
        }
        let err = Store::open(&db_path, Vault::from_key(&[1; 32]))
            .err()
            .expect("newer version must fail");
        assert!(err.contains("newer Alyae"), "unexpected error: {err}");
        assert_eq!(user_version(&db_path), 99);
        assert!(!dir.join("t.db-wal").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_secret_row_is_skipped_and_reported() {
        let dir = temp_dir("corrupt-row");
        let db_path = dir.join("t.db");
        let s = Store::open(&db_path, Vault::from_key(&[1; 32])).unwrap();
        let mut good = Account {
            id: "good1".into(),
            game: "generic".into(),
            name: "good".into(),
            ..Default::default()
        };
        good.secrets.insert("password".into(), "ok".into());
        s.save(&good).unwrap();

        let bad_blob = Vault::from_key(&[2; 32]).seal(b"{\"password\":\"wrongkey\"}");
        s.db.execute(
            "INSERT INTO accounts (id, game, name, secrets) VALUES ('bad1', 'generic', 'bad', ?1)",
            params![bad_blob],
        )
        .unwrap();

        let (accounts, problems) = s.list().unwrap();
        assert_eq!(accounts, vec![good]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("bad1"), "got: {}", problems[0]);
        assert!(
            problems[0].contains("cannot decrypt secrets"),
            "got: {}",
            problems[0]
        );

        // Bad row stays in the database untouched.
        let count: i64 =
            s.db.query_row("SELECT count(*) FROM accounts WHERE id = 'bad1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
