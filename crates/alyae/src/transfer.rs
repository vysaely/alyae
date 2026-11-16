//! Accounts in one file, to take to another PC (or keep as a backup): a `.alyae` file sealed with
//! a password the person types, so the file alone gives nothing away.
//!
//! Layout: one JSON header line `{"alyae_export":1,"kdf":"argon2id","m":..,"t":..,"p":..,"salt":hex}`
//! then the sealed accounts in hex: XSalsa20-Poly1305 (the vault's cipher) under the key
//! Argon2id makes from the password and salt. The header is authenticated as part of the
//! sealed data, so changing it (weaker settings) makes the file refuse to open.

use crate::store::{Account, Store, Values};
use crate::vault::Vault;
use serde::{Deserialize, Serialize};

const FORMAT: u32 = 1;
/// Argon2id settings: 64 MiB, 3 passes (OWASP's first choice); about a second on a laptop.
const M_KIB: u32 = 64 * 1024;
const T: u32 = 3;
const P: u32 = 1;
/// The fewest a header may ask for: a file asking for less was not written by Alyae.
const MIN_M_KIB: u32 = 19 * 1024;
pub const MIN_PASSWORD: usize = 8;
/// The most a header may ask for.
const MAX_M_KIB: u32 = 256 * 1024;
const MAX_T: u32 = 10;
const MAX_P: u32 = 4;

#[derive(Serialize, Deserialize)]
struct Header {
    alyae_export: u32,
    kdf: String,
    m: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct Item {
    id: String,
    game: String,
    name: String,
    group: String,
    notes: String,
    sort: i64,
    created: f64,
    last_used: f64,
    fields: Values,
    secrets: Values,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn key(password: &str, h: &Header) -> Result<[u8; 32], String> {
    let salt = unhex(&h.salt).ok_or("the file is damaged (salt)")?;
    let params = argon2::Params::new(h.m, h.t, h.p, Some(32)).map_err(|e| e.to_string())?;
    let mut out = [0u8; 32];
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(password.as_bytes(), &salt, &mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// The file's text for `accounts`, sealed with `password`.
pub fn export(accounts: &[Account], password: &str) -> Result<String, String> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!(
            "the password needs at least {MIN_PASSWORD} characters"
        ));
    }
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    let h = Header {
        alyae_export: FORMAT,
        kdf: "argon2id".into(),
        m: M_KIB,
        t: T,
        p: P,
        salt: hex(&salt),
    };
    let header = serde_json::to_string(&h).map_err(|e| e.to_string())?;
    let items: Vec<Item> = accounts
        .iter()
        .map(|a| Item {
            id: a.id.clone(),
            game: a.game.clone(),
            name: a.name.clone(),
            group: a.group.clone(),
            notes: a.notes.clone(),
            sort: a.sort,
            created: a.created,
            last_used: a.last_used,
            fields: a.fields.clone(),
            secrets: a.secrets.clone(),
        })
        .collect();
    // the header goes inside too: a file whose header was changed does not open
    let body = serde_json::json!({ "header": header, "accounts": items }).to_string();
    let sealed = Vault::from_key(&key(password, &h)?).seal(body.as_bytes());
    Ok(format!("{header}\n{}\n", hex(&sealed)))
}

/// The accounts in a file's `text`, opened with `password`.
pub fn read(text: &str, password: &str) -> Result<Vec<Account>, String> {
    let (header, sealed) = text
        .trim()
        .split_once('\n')
        .ok_or("this is not an Alyae accounts file")?;
    let h: Header =
        serde_json::from_str(header).map_err(|_| "this is not an Alyae accounts file")?;
    if h.alyae_export != FORMAT || h.kdf != "argon2id" {
        return Err("the file is from a newer Alyae".into());
    }
    // the settings are read before anything is checked: a file asking for more than Alyae's own
    // (64 MiB, 3 passes) by a wide margin is refused rather than freezing or exhausting the PC
    if h.m < MIN_M_KIB || h.t < 2 || h.m > MAX_M_KIB || h.t > MAX_T || h.p == 0 || h.p > MAX_P {
        return Err("the file's settings are not ones Alyae writes".into());
    }
    let blob = unhex(sealed).ok_or("the file is damaged")?;
    let plain = Vault::from_key(&key(password, &h)?)
        .open_blob(&blob)
        .ok_or("wrong password, or the file is damaged")?;
    let v: serde_json::Value = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
    if v["header"].as_str() != Some(header.trim()) {
        return Err("the file was changed after it was written".into());
    }
    let items: Vec<Item> =
        serde_json::from_value(v["accounts"].clone()).map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|i| Account {
            id: i.id,
            game: i.game,
            name: i.name,
            group: i.group,
            notes: i.notes,
            sort: i.sort,
            created: i.created,
            last_used: i.last_used,
            fields: i.fields,
            secrets: i.secrets,
        })
        .collect())
}

#[derive(Default)]
pub struct Imported {
    pub added: usize,
    pub skipped: usize,
    /// accounts whose secrets could not all be opened (AEAM's), with how many
    pub unreadable: Vec<(String, usize)>,
    /// names of added accounts that carry a command to run (a "launch" field): a file from
    /// someone else could hide anything there, so the window names them
    pub with_command: Vec<String>,
}

/// Whether `a` is already among `have`: the same account ID, or the same Roblox user.
fn already(have: &[Account], a: &Account) -> bool {
    have.iter().any(|h| {
        h.id == a.id
            || (h.game == a.game
                && !a.value("user_id").is_empty()
                && h.value("user_id") == a.value("user_id"))
    })
}

/// Adds the accounts of a file to `store`, skipping ones it has; all or nothing.
pub fn import(store: &Store, have: &[Account], accounts: Vec<Account>) -> Result<Imported, String> {
    store.transaction(|| {
        let mut r = Imported::default();
        let mut seen: Vec<Account> = have.to_vec();
        for mut a in accounts {
            // a folder of this PC's, chosen by Alyae: one from a file could name any folder,
            // which Join would write into and Reset files would empty
            a.fields.remove("profile");
            if a.id.is_empty() || a.game.is_empty() || already(&seen, &a) {
                r.skipped += 1;
                continue;
            }
            store.save(&a)?;
            if !a.value("launch").trim().is_empty() {
                r.with_command.push(a.name.clone());
            }
            r.added += 1;
            seen.push(a);
        }
        Ok(r)
    })
}

/// Another manager's accounts, by name: Roblox Account Manager's AccountData.json (or its backup)
/// or AEAM's manager.db.
pub fn foreign(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|f| f.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|f| {
            matches!(
                f.as_str(),
                "accountdata.json"
                    | "accountdata.json.backup"
                    | "accountdata.json.bak"
                    | "manager.db"
            )
        })
}

/// Where a new export goes by default: Documents (else home), named by today's date.
pub fn default_path() -> std::path::PathBuf {
    let day = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86400);
    let (y, m, d) = crate::log::civil(day as i64);
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_default()
        .join(format!("alyae-accounts-{y:04}-{m:02}-{d:02}.alyae"))
}

/// Writes `text` to `path` readable by its owner only (on Linux), never over an existing file.
pub fn write_new(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} exists already", path.display()),
        _ => format!("cannot write {}: {e}", path.display()),
    })?;
    f.write_all(text.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| {
            // a half-written file would only make the next try say it exists already
            drop(f);
            let _ = std::fs::remove_file(path);
            format!("cannot write {}: {e}", path.display())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(id: &str, game: &str, name: &str) -> Account {
        let mut a = Account {
            id: id.into(),
            game: game.into(),
            name: name.into(),
            ..Default::default()
        };
        a.secrets.insert("password".into(), format!("pw-{id}"));
        a
    }

    #[test]
    fn export_then_read_gives_the_same_accounts() {
        let list = vec![acc("a1", "roblox", "One"), acc("a2", "generic", "Two")];
        let text = export(&list, "correct horse").unwrap();
        assert!(!text.contains("pw-a1"), "a secret is in the file in clear");
        assert_eq!(read(&text, "correct horse").unwrap(), list);
        assert!(
            read(&text, "wrong horse!")
                .unwrap_err()
                .contains("wrong password")
        );
    }

    #[test]
    fn a_changed_header_or_short_password_is_refused() {
        assert!(export(&[], "short").is_err());
        let text = export(&[acc("a1", "roblox", "One")], "correct horse").unwrap();
        // a weaker setting written into the header
        let weaker = text.replacen("\"t\":3", "\"t\":2", 1);
        assert!(read(&weaker, "correct horse").is_err());
        let huge = text.replacen("\"m\":65536", "\"m\":4194304", 1);
        assert!(
            read(&huge, "correct horse")
                .unwrap_err()
                .contains("settings")
        );
        let tiny = text.replacen("\"m\":65536", "\"m\":8", 1);
        assert!(
            read(&tiny, "correct horse")
                .unwrap_err()
                .contains("settings")
        );
        assert!(read("hello\nworld", "correct horse").is_err());
    }

    /// A file's accounts come in once: a second import of it, or an account already here (same
    /// ID, or the same Roblox user), is skipped; accounts with a launch command are named.
    #[test]
    fn import_skips_what_is_here_and_names_commands() {
        let dir = std::env::temp_dir().join(format!("alyae-transfer-{}", crate::store::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("a.db"), Vault::from_key(&[3u8; 32])).unwrap();
        let mut here = acc("h1", "roblox", "Here");
        here.fields.insert("user_id".into(), "77".into());
        store.save(&here).unwrap();
        let mut same_user = acc("x1", "roblox", "Same user");
        same_user.fields.insert("user_id".into(), "77".into());
        let mut cmd = acc("g1", "generic", "With command");
        cmd.fields
            .insert("profile".into(), "/home/someone/.mozilla".into());
        cmd.fields.insert("launch".into(), "calc.exe".into());
        let file = vec![same_user, cmd, acc("n1", "roblox", "New")];
        let have = store.list().unwrap().0;
        let r = import(&store, &have, file.clone()).unwrap();
        assert_eq!((r.added, r.skipped), (2, 1));
        assert_eq!(r.with_command, ["With command"]);
        let have = store.list().unwrap().0;
        let again = import(&store, &have, file).unwrap();
        assert_eq!((again.added, again.skipped), (0, 3));
        let g1 = have.iter().find(|a| a.id == "g1").unwrap();
        assert_eq!(
            g1.value("profile"),
            "",
            "an imported profile folder was kept"
        );
        // secrets come back sealed under this PC's own key
        let n1 = have.iter().find(|a| a.id == "n1").unwrap();
        assert_eq!(n1.value("password"), "pw-n1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dates_are_civil() {
        use crate::log::civil;
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_773), (2026, 11, 16));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }
}
