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

impl Store {
    pub fn open(db_path: &Path, vault: Vault) -> Result<Self, String> {
        let db = Connection::open(db_path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600));
        }
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS accounts (
                 id TEXT PRIMARY KEY, game TEXT NOT NULL, name TEXT NOT NULL,
                 grp TEXT NOT NULL DEFAULT '', notes TEXT NOT NULL DEFAULT '',
                 sort INTEGER NOT NULL DEFAULT 0, created REAL NOT NULL DEFAULT 0,
                 last_used REAL NOT NULL DEFAULT 0, fields TEXT NOT NULL DEFAULT '{}',
                 secrets BLOB);
             CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { db, vault })
    }

    pub fn list(&self) -> Result<Vec<Account>, String> {
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
        for row in rows {
            let (mut a, fields, blob) = row.map_err(|e| e.to_string())?;
            a.fields = serde_json::from_str(&fields)
                .map_err(|e| format!("the fields of '{}' are damaged: {e}", a.name))?;
            if let Some(b) = blob {
                let plain = self.vault.open_blob(&b).ok_or_else(|| {
                    format!("the secrets of '{}' do not open with this key", a.name)
                })?;
                a.secrets = serde_json::from_slice(&plain)
                    .map_err(|e| format!("the secrets of '{}' are damaged: {e}", a.name))?;
            }
            out.push(a);
        }
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
        let _ = self.db.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value=?2",
            [key, value],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("ludae-store-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        assert_eq!(s.list().unwrap(), vec![a.clone()]);
        s.delete(&a.id).unwrap();
        assert!(s.list().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
