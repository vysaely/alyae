//! The few Roblox web calls Alyae needs.

use serde_json::{Value, json};
use std::time::Duration;

const UA: &str = "Alyae/0.1 (game account manager)";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(UA)
        .build()
        .into()
}

/// The token itself from a pasted cookie (`.ROBLOSECURITY=...; path=...`, quoted, or bare).
pub fn clean_cookie(raw: &str) -> String {
    let raw = raw.trim();
    let v = match raw.split_once(".ROBLOSECURITY=") {
        Some((_, rest)) => rest.split(';').next().unwrap_or("").trim(),
        None => raw,
    };
    v.trim_matches(['"', '\'']).to_string()
}

fn net(e: ureq::Error) -> String {
    format!("cannot reach Roblox: {e}")
}

pub fn answer(status: u16, what: &str) -> String {
    match status {
        401 => "the cookie has expired: sign in again and paste the new one".into(),
        429 => "Roblox is rate limiting: try again in a minute".into(),
        s => format!("{what}: Roblox answered {s}"),
    }
}

/// (user ID, username) of the cookie's account.
pub fn authenticated(cookie: &str) -> Result<(u64, String), String> {
    let mut r = agent()
        .get("https://users.roblox.com/v1/users/authenticated")
        .header("Cookie", &format!(".ROBLOSECURITY={cookie}"))
        .header("Accept", "application/json")
        .call()
        .map_err(net)?;
    if r.status() != 200 {
        return Err(answer(r.status().as_u16(), "sign-in check"));
    }
    let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
    let id = v["id"].as_u64().ok_or("Roblox sent no user ID")?;
    Ok((id, v["name"].as_str().unwrap_or("").to_string()))
}

pub fn user_id_of(username: &str) -> Result<u64, String> {
    let mut r = agent()
        .post("https://users.roblox.com/v1/usernames/users")
        .send_json(json!({ "usernames": [username], "excludeBannedUsers": false }))
        .map_err(net)?;
    if r.status() != 200 {
        return Err(answer(r.status().as_u16(), "user lookup"));
    }
    let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
    v["data"][0]["id"]
        .as_u64()
        .ok_or_else(|| format!("no Roblox user is called '{username}'"))
}

/// Who a request is sent as: the cookie, the device ID (some APIs refuse requests without one)
/// and the 2FA secret that answers Roblox's two-step challenge.
#[derive(Default)]
pub struct Session {
    pub cookie: String,
    pub tracker: Option<String>,
    pub totp: String,
}

impl Session {
    pub fn plain(cookie: &str) -> Self {
        Self {
            cookie: cookie.to_string(),
            ..Default::default()
        }
    }

    fn cookie_header(&self) -> String {
        match &self.tracker {
            Some(t) => format!(".ROBLOSECURITY={}; RBXEventTrackerV2={t}", self.cookie),
            None => format!(".ROBLOSECURITY={}", self.cookie),
        }
    }
}

pub type Response = ureq::http::Response<ureq::Body>;

fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.headers().get(name).and_then(|v| v.to_str().ok())
}

/// A signed POST (or GET with `body` None): fetches the CSRF token Roblox asks for, waits once
/// when rate limited, and answers a two-step challenge with the session's 2FA secret.
pub fn call(url: &str, s: &Session, body: Option<&Value>) -> Result<Response, String> {
    let a = agent();
    let mut csrf = String::new();
    let mut challenge: Option<[(&str, String); 3]> = None;
    let mut waited = false;
    for _ in 0..5 {
        let r = match body {
            None => {
                let mut q = a.get(url).header("Accept", "application/json");
                // no cookie: no Cookie header at all, so the request is anonymous
                if !s.cookie.is_empty() {
                    q = q.header("Cookie", &s.cookie_header());
                }
                if let Some(c) = &challenge {
                    for (k, v) in c {
                        q = q.header(*k, v);
                    }
                }
                q.call()
            }
            Some(b) => {
                let mut q = a
                    .post(url)
                    .header("Accept", "application/json")
                    .header("Referer", "https://www.roblox.com/")
                    .header("Origin", "https://www.roblox.com");
                if !s.cookie.is_empty() {
                    q = q.header("Cookie", &s.cookie_header());
                }
                if !csrf.is_empty() {
                    q = q.header("X-CSRF-TOKEN", &csrf);
                }
                if let Some(c) = &challenge {
                    for (k, v) in c {
                        q = q.header(*k, v);
                    }
                }
                q.send_json(b)
            }
        }
        .map_err(net)?;
        let status = r.status().as_u16();
        if status == 429 && !waited {
            let secs = header(&r, "retry-after")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(15)
                .min(60);
            std::thread::sleep(Duration::from_secs(secs));
            waited = true;
            continue;
        }
        if status != 403 {
            return Ok(r);
        }
        if header(&r, "rblx-challenge-type")
            .is_some_and(|t| t.eq_ignore_ascii_case("twostepverification"))
            && challenge.is_none()
        {
            challenge = Some(two_step(&r, s)?);
            continue;
        }
        match header(&r, "x-csrf-token") {
            Some(t) if t != csrf => csrf = t.to_string(),
            _ => return Ok(r),
        }
    }
    Err("Roblox kept refusing the request".into())
}

/// Answers a two-step challenge with the current code from the 2FA secret; returns the headers
/// that prove it on the retried request.
fn two_step(r: &Response, s: &Session) -> Result<[(&'static str, String); 3], String> {
    if s.totp.is_empty() {
        return Err("Roblox asks for a 2FA code: save the account's 2FA secret first".into());
    }
    let id = header(r, "rblx-challenge-id")
        .ok_or("Roblox's challenge has no ID")?
        .to_string();
    let raw = header(r, "rblx-challenge-metadata").ok_or("Roblox's challenge has no details")?;
    let meta: Value =
        serde_json::from_slice(&base64_decode(raw).ok_or("unreadable challenge details")?)
            .map_err(|e| format!("unreadable challenge details: {e}"))?;
    let user = meta["userId"]
        .as_str()
        .map(String::from)
        .or_else(|| meta["userId"].as_u64().map(|u| u.to_string()))
        .ok_or("the challenge names no user")?;
    let sub = meta["challengeId"]
        .as_str()
        .ok_or("the challenge has no inner ID")?
        .to_string();
    let action = meta["actionType"].as_str().unwrap_or("Generic").to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let code = alyae_otp::code(&s.totp, now).ok_or("the saved 2FA secret is not valid")?;
    let plain = Session {
        cookie: s.cookie.clone(),
        tracker: s.tracker.clone(),
        totp: String::new(),
    };
    let mut v = call(
        &format!(
            "https://twostepverification.roblox.com/v1/users/{user}/challenges/authenticator/verify"
        ),
        &plain,
        Some(&json!({ "actionType": action, "challengeId": sub, "code": code })),
    )?;
    if v.status() != 200 {
        let body: Value = v.body_mut().read_json().unwrap_or(Value::Null);
        let msg = body["errors"][0]["message"]
            .as_str()
            .unwrap_or("rejected")
            .to_string();
        return Err(format!("2FA code {msg}"));
    }
    let token = v
        .body_mut()
        .read_json::<Value>()
        .map_err(|e| e.to_string())?["verificationToken"]
        .as_str()
        .ok_or("Roblox sent no verification token")?
        .to_string();
    let inner = json!({ "verificationToken": token, "rememberDevice": false, "challengeId": sub, "actionType": action });
    let cont = call(
        "https://apis.roblox.com/challenge/v1/continue",
        &plain,
        Some(
            &json!({ "challengeId": id, "challengeType": "twostepverification", "challengeMetadata": inner.to_string() }),
        ),
    )?;
    if cont.status() != 200 {
        return Err(answer(cont.status().as_u16(), "2FA challenge"));
    }
    Ok([
        ("rblx-challenge-id", id),
        ("rblx-challenge-type", "twostepverification".into()),
        (
            "rblx-challenge-metadata",
            base64_encode(inner.to_string().as_bytes()),
        ),
    ])
}

/// The new `.ROBLOSECURITY` a response sets (a sign-out everywhere or a password change).
pub fn new_cookie(r: &Response) -> Option<String> {
    r.headers().get_all("set-cookie").iter().find_map(|h| {
        let v = h.to_str().ok()?.strip_prefix(".ROBLOSECURITY=")?;
        let v = v.split(';').next()?.trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let (mut buf, mut bits, mut out) = (0u32, 0, Vec::new());
    for c in s.trim().bytes().filter(|&c| c != b'=') {
        buf = buf << 6 | B64.iter().position(|&b| b == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// (place ID, link code) of a `roblox.com/share?code=...&type=Server` link.
pub fn resolve_share_link(link: &str, cookie: &str) -> Result<(u64, String), String> {
    let code = link
        .split(['?', '&'])
        .find_map(|p| p.strip_prefix("code="))
        .ok_or("the link has no code")?;
    let mut r = call(
        "https://apis.roblox.com/sharelinks/v1/resolve-link",
        &Session::plain(cookie),
        Some(&json!({ "linkId": code, "linkType": "Server" })),
    )?;
    if r.status() != 200 {
        return Err(answer(r.status().as_u16(), "share link"));
    }
    let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
    let d = &v["privateServerInviteData"];
    match (d["placeId"].as_u64(), d["linkCode"].as_str()) {
        (Some(p), Some(c)) => Ok((p, c.to_string())),
        _ => Err("the link is not a private server, or it has expired".into()),
    }
}

/// Online status by user ID, for up to 100 users per request, as the account with `cookie` sees
/// it (an empty cookie asks anonymously: the status Roblox shows to everyone): "Offline",
/// "Online", "In game: <place>", "In Studio", "Invisible".
pub fn presence(user_ids: &[u64], cookie: &str) -> Result<Vec<(u64, String)>, String> {
    let mut out = Vec::new();
    for chunk in user_ids.chunks(100) {
        let mut r = call(
            "https://presence.roblox.com/v1/presence/users",
            &Session::plain(cookie),
            Some(&json!({ "userIds": chunk })),
        )?;
        if r.status() != 200 {
            return Err(answer(r.status().as_u16(), "online status"));
        }
        let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
        for p in v["userPresences"].as_array().into_iter().flatten() {
            let Some(id) = p["userId"].as_u64() else {
                continue;
            };
            let place = p["lastLocation"].as_str().unwrap_or("").trim();
            let text = match p["userPresenceType"].as_u64() {
                Some(1) => "Online".to_string(),
                Some(2) if place.is_empty() => "In game".to_string(),
                Some(2) => format!("In game: {place}"),
                Some(3) => "In Studio".to_string(),
                Some(4) => "Invisible".to_string(),
                _ => "Offline".to_string(),
            };
            out.push((id, text));
        }
    }
    Ok(out)
}

/// Where `user` is playing, as the account with `cookie` may see it: (place ID, server ID).
pub fn whereabouts(user: u64, cookie: &str) -> Result<Option<(u64, String)>, String> {
    let mut r = call(
        "https://presence.roblox.com/v1/presence/users",
        &Session::plain(cookie),
        Some(&json!({ "userIds": [user] })),
    )?;
    if r.status() != 200 {
        return Err(answer(r.status().as_u16(), "online status"));
    }
    let v: Value = r.body_mut().read_json().map_err(|e| e.to_string())?;
    let p = &v["userPresences"][0];
    Ok(match (p["placeId"].as_u64(), p["gameId"].as_str()) {
        (Some(place), Some(server)) if place > 0 && !server.is_empty() => {
            Some((place, server.to_string()))
        }
        _ => None,
    })
}

/// A new device identity (`RBXEventTrackerV2` value), as the Roblox app makes on its first start.
/// Some requests (blocking a player, for one) fail from sessions without one.
pub fn new_browser_tracker() -> Option<String> {
    let r = agent()
        .post("https://apis.roblox.com/browser-tracker-api/device/initialize")
        .header("Accept", "application/json")
        .send_json(json!({}))
        .ok()?;
    r.headers().get_all("set-cookie").iter().find_map(|h| {
        let v = h.to_str().ok()?.strip_prefix("RBXEventTrackerV2=")?;
        let v = v.split(';').next()?.trim();
        browser_id(v).is_some().then(|| v.to_string())
    })
}

/// The numeric `browserid` inside a tracker value.
pub fn browser_id(tracker: &str) -> Option<&str> {
    tracker
        .split('&')
        .find_map(|f| f.strip_prefix("browserid="))
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 20
                && v.bytes().all(|b| b.is_ascii_digit())
                && !v.starts_with('0')
        })
}

/// A one-time ticket that signs the Windows client in.
#[cfg(windows)]
pub fn auth_ticket(cookie: &str) -> Result<String, String> {
    let r = call(
        "https://auth.roblox.com/v1/authentication-ticket",
        &Session::plain(cookie),
        Some(&json!({})),
    )?;
    if let Some(t) = r
        .headers()
        .get("rbx-authentication-ticket")
        .and_then(|t| t.to_str().ok())
    {
        return Ok(t.to_string());
    }
    Err(answer(r.status().as_u16(), "sign-in ticket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for s in ["", "a", "ab", "abc", "{\"challengeId\":\"x\"}"] {
            assert_eq!(
                base64_decode(&base64_encode(s.as_bytes())).unwrap(),
                s.as_bytes()
            );
        }
        assert_eq!(base64_encode(b"Ma"), "TWE=");
    }

    #[test]
    fn cookies_and_trackers() {
        assert_eq!(
            clean_cookie(" .ROBLOSECURITY=\"_|WARNING|abc\"; path=/ "),
            "_|WARNING|abc"
        );
        assert_eq!(clean_cookie("'_|abc'"), "_|abc");
        assert_eq!(
            browser_id("CreateDate=1/1/2026&rbxid=&browserid=1234567"),
            Some("1234567")
        );
        assert_eq!(browser_id("browserid=0123"), None);
    }
}
