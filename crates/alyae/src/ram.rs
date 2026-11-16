//! Roblox Account Manager's `AccountData.json` (ic3w0lf22's RAM), read for Bring in. RAM keeps it
//! one of three ways (RBX Alt Manager/AccountManager.cs, Classes/Cryptography.cs):
//! - with a password: RAM's header, a 16-byte salt, a 24-byte nonce, then a libsodium secretbox
//!   under the key libsodium's Argon2 ("moderate": 6 passes, 128 MiB) makes from the SHA-512 of
//!   the password;
//! - by default: Windows' DPAPI with RAM's entropy, which only that Windows user can open;
//! - in plain JSON (with RAM's "NoEncryption" file).
//!
//! In every case the content is a JSON list of RAM's accounts.

use crate::store::{Account, Values, new_id};
use crypto_secretbox::aead::{Aead, KeyInit};
use crypto_secretbox::{Key, Nonce, XSalsa20Poly1305};
use serde::Deserialize;

/// "Roblox Account Manager created by ic3w0lf22 @ github.com ......."
const HEADER: &[u8] = b"Roblox Account Manager created by ic3w0lf22 @ github.com .......";

/// RAM's DPAPI entropy ("ROBLOX ACCOUNT MANAGER | :) | BROUGHT TO YOU BUY ic3w0lf").
#[cfg(any(windows, test))]
const ENTROPY: &[u8] = b"ROBLOX ACCOUNT MANAGER | :) | BROUGHT TO YOU BUY ic3w0lf";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RamAccount {
    #[serde(default)]
    security_token: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default, rename = "UserID")]
    user_id: Option<i64>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

/// Whether `bytes` look like RAM's file: its password header, or its JSON list.
pub fn is_ram(bytes: &[u8]) -> bool {
    bytes.starts_with(HEADER) || parse(bytes).is_ok_and(|l| !l.is_empty())
}

/// RAM's accounts in `bytes`, opened with `password` where RAM sealed them with one.
pub fn read(bytes: &[u8], password: &str) -> Result<Vec<Account>, String> {
    if bytes.starts_with(HEADER) {
        if password.is_empty() {
            return Err("this Roblox Account Manager file has a password: type it in".into());
        }
        return parse(&open_sealed(bytes, password)?);
    }
    if let Ok(list) = parse(bytes) {
        return Ok(list);
    }
    unprotect(bytes).and_then(|plain| parse(&plain))
}

fn parse(bytes: &[u8]) -> Result<Vec<Account>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not text")?;
    let list: Vec<RamAccount> =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())?;
    Ok(list.into_iter().filter_map(account).collect())
}

/// One RAM account as Alyae's Roblox account; one without a cookie is left out.
fn account(r: RamAccount) -> Option<Account> {
    let cookie = r.security_token.filter(|c| !c.trim().is_empty())?;
    let mut fields = Values::new();
    let mut secrets = Values::new();
    if let Some(u) = r.username.filter(|u| !u.is_empty()) {
        fields.insert("username".into(), u);
    }
    if let Some(id) = r.user_id.filter(|id| *id > 0) {
        fields.insert("user_id".into(), id.to_string());
    }
    secrets.insert("cookie".into(), cookie.trim().to_string());
    if let Some(p) = r.password.filter(|p| !p.is_empty()) {
        secrets.insert("password".into(), p);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    Some(Account {
        id: new_id(),
        game: "roblox".into(),
        name: r.alias.unwrap_or_default(),
        group: r.group.filter(|g| g != "Default").unwrap_or_default(),
        notes: r.description.unwrap_or_default(),
        created: now,
        fields,
        secrets,
        ..Default::default()
    })
}

/// RAM's password-sealed file opened. libsodium-net's `ArgonHashBinary` is Argon2i in the version
/// RAM ships; Argon2id is tried as well, for a build made with a newer libsodium-net.
fn open_sealed(bytes: &[u8], password: &str) -> Result<Vec<u8>, String> {
    use sha2::Digest;
    let rest = &bytes[HEADER.len()..];
    if rest.len() < 16 + 24 + 16 {
        return Err("the Roblox Account Manager file is cut short".into());
    }
    let (salt, rest) = rest.split_at(16);
    let (nonce, sealed) = rest.split_at(24);
    let hash = sha2::Sha512::digest(password.as_bytes());
    for alg in [argon2::Algorithm::Argon2i, argon2::Algorithm::Argon2id] {
        let params = argon2::Params::new(128 * 1024, 6, 1, Some(32)).map_err(|e| e.to_string())?;
        let mut key = [0u8; 32];
        argon2::Argon2::new(alg, argon2::Version::V0x13, params)
            .hash_password_into(&hash, salt, &mut key)
            .map_err(|e| e.to_string())?;
        if let Ok(plain) =
            XSalsa20Poly1305::new(Key::from_slice(&key)).decrypt(Nonce::from_slice(nonce), sealed)
        {
            return Ok(plain);
        }
    }
    Err("wrong password for this Roblox Account Manager file".into())
}

/// RAM's default file, opened with Windows' DPAPI as the Windows user who saved it.
#[cfg(windows)]
fn unprotect(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};
    let blob = |b: &[u8]| CRYPT_INTEGER_BLOB {
        cbData: b.len() as u32,
        pbData: b.as_ptr() as *mut u8,
    };
    let input = blob(bytes);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: the in-blobs point at live slices; Windows allocates `out`, freed below
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(LOCKED.into());
    }
    // SAFETY: Windows wrote cbData bytes at pbData
    let plain = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData.cast()) };
    Ok(plain)
}

#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err(LOCKED.into())
}

const LOCKED: &str = "this Roblox Account Manager file is locked to the Windows PC and user that \
    saved it: bring it in with Alyae on that PC, or give it a password in Roblox Account Manager \
    first (Settings, Encryption)";

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"[{"Valid":true,"SecurityToken":"_|WARNING:-DO-NOT-SHARE-THIS.--abc","Username":"Player1","LastUse":"2026-01-02T03:04:05","Alias":"Main","Description":"Druid","Password":"pw1","Group":"Arch","UserID":77,"Fields":{},"LastAttemptedRefresh":"2026-01-01T00:00:00","BrowserTrackerID":"123"},
        {"SecurityToken":"tok2","Username":"Player2","Group":"Default","UserID":78},
        {"SecurityToken":null,"Username":"NoCookie"}]"#;

    #[test]
    fn plain_file_reads() {
        assert_eq!(&ENTROPY[..6], &[0x52, 0x4f, 0x42, 0x4c, 0x4f, 0x58]);
        assert_eq!(HEADER.len(), 64);
        let list = read(JSON.as_bytes(), "").unwrap();
        assert_eq!(list.len(), 2, "the account without a cookie is left out");
        let a = &list[0];
        assert_eq!(
            (
                a.game.as_str(),
                a.name.as_str(),
                a.group.as_str(),
                a.notes.as_str()
            ),
            ("roblox", "Main", "Arch", "Druid")
        );
        assert_eq!(a.value("username"), "Player1");
        assert_eq!(a.value("user_id"), "77");
        assert_eq!(a.secrets["cookie"], "_|WARNING:-DO-NOT-SHARE-THIS.--abc");
        assert_eq!(a.secrets["password"], "pw1");
        // RAM's "Default" group is no group
        assert_eq!(list[1].group, "");
        assert!(is_ram(JSON.as_bytes()));
        assert!(!is_ram(b"{\"alyae_export\":1}"));
    }

    /// A file sealed as RAM seals it (libsodium's argon2i "moderate", secretbox) opens with the
    /// password and not without it.
    #[test]
    fn password_file_opens() {
        use sha2::Digest;
        let salt = [7u8; 16];
        let nonce = [9u8; 24];
        let params = argon2::Params::new(128 * 1024, 6, 1, Some(32)).unwrap();
        let mut key = [0u8; 32];
        argon2::Argon2::new(argon2::Algorithm::Argon2i, argon2::Version::V0x13, params)
            .hash_password_into(&sha2::Sha512::digest(b"hunter22"), &salt, &mut key)
            .unwrap();
        let sealed = XSalsa20Poly1305::new(Key::from_slice(&key))
            .encrypt(Nonce::from_slice(&nonce), JSON.as_bytes())
            .unwrap();
        let file = [HEADER, &salt, &nonce, &sealed].concat();
        assert!(is_ram(&file));
        assert_eq!(read(&file, "hunter22").unwrap().len(), 2);
        assert!(read(&file, "wrong").unwrap_err().contains("wrong password"));
        assert!(read(&file, "").unwrap_err().contains("password"));
    }

    #[cfg(not(windows))]
    #[test]
    fn windows_locked_file_says_so() {
        assert!(
            read(&[1, 0, 0, 0, 0xd0, 0x8c, 0x9d], "")
                .unwrap_err()
                .contains("locked")
        );
    }
}
