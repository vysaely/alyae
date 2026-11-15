//! Secrets at rest: XSalsa20-Poly1305 (NaCl secretbox) under a random 32-byte key kept in a file
//! only the owner can read. A sealed blob is nonce (24 bytes) followed by the ciphertext, the same
//! layout AEAM wrote with libsodium, so AEAM's blobs open here unchanged.

use crypto_secretbox::aead::{Aead, KeyInit};
use crypto_secretbox::{Key, Nonce, XSalsa20Poly1305};
use std::fs;
use std::io::Write;
use std::path::Path;

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

pub struct Vault {
    cipher: XSalsa20Poly1305,
}

impl Vault {
    /// Opens the key at `path`, creating it when missing. A key file of the wrong size is an
    /// error, never replaced: everything sealed under the old key would be lost.
    pub fn open(path: &Path) -> Result<Self, String> {
        let key = match fs::read(path) {
            Ok(k) if k.len() == KEY_LEN => k,
            Ok(k) => {
                return Err(format!(
                    "{} holds {} bytes, not a key",
                    path.display(),
                    k.len()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_key(path)?,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        Ok(Self::from_key(&key))
    }

    pub fn from_key(key: &[u8]) -> Self {
        Self {
            cipher: XSalsa20Poly1305::new(Key::from_slice(key)),
        }
    }

    pub fn seal(&self, plain: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).expect("system random source");
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plain)
            .expect("secretbox encrypt");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        out
    }

    pub fn open_blob(&self, blob: &[u8]) -> Option<Vec<u8>> {
        if blob.len() < NONCE_LEN + 16 {
            return None;
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        self.cipher.decrypt(Nonce::from_slice(nonce), ct).ok()
    }
}

fn create_key(path: &Path) -> Result<Vec<u8>, String> {
    let mut key = vec![0u8; KEY_LEN];
    getrandom::fill(&mut key).map_err(|e| e.to_string())?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    match opts.open(path) {
        Ok(mut f) => {
            f.write_all(&key)
                .and_then(|_| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(key)
        }
        // another Alyae started at the same moment and made it first: use theirs
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::read(path).map_err(|e| e.to_string())
        }
        Err(e) => Err(format!("cannot create {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_then_open() {
        let v = Vault::from_key(&[7u8; 32]);
        let blob = v.seal(b"secret");
        assert_eq!(v.open_blob(&blob).as_deref(), Some(&b"secret"[..]));
        let other = Vault::from_key(&[8u8; 32]);
        assert!(other.open_blob(&blob).is_none());
    }
}
