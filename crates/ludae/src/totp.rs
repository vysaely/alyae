//! RFC 6238 codes (SHA-1, 30 s, 6 digits) from a base32 secret or an otpauth:// link.

use hmac::{Hmac, Mac};
use sha1::Sha1;

/// The base32 secret inside `input`: the input itself, or the `secret=` of an otpauth:// link.
fn secret_of(input: &str) -> String {
    let s = input.trim();
    let s = if s.starts_with("otpauth://") {
        s.split(['?', '&'])
            .find_map(|p| p.strip_prefix("secret="))
            .unwrap_or("")
    } else {
        s
    };
    s.chars().filter(|c| !c.is_whitespace() && *c != '=' && *c != '-').collect::<String>().to_ascii_uppercase()
}

fn base32(s: &str) -> Option<Vec<u8>> {
    const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut buf, mut bits, mut out) = (0u64, 0u32, Vec::new());
    for c in s.bytes() {
        let v = ABC.iter().position(|&a| a == c)? as u64;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The code for unix time `now`, or None when `secret` is not valid base32.
pub fn code(secret: &str, now: u64) -> Option<String> {
    let key = base32(&secret_of(secret))?;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&(now / 30).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[19] & 0xf) as usize;
    let n = u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]]) % 1_000_000;
    Some(format!("{n:06}"))
}

pub fn seconds_left(now: u64) -> u64 {
    30 - now % 30
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_vector() {
        // RFC 6238 SHA-1 key "12345678901234567890", T = 59 -> 94287082 (8 digits) -> 287082
        let secret = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
        assert_eq!(code(secret, 59).as_deref(), Some("287082"));
        let link = format!("otpauth://totp/x?secret={secret}&issuer=y");
        assert_eq!(code(&link, 59).as_deref(), Some("287082"));
        assert_eq!(code("not base32!", 59), None);
    }
}
