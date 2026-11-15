//! The few Roblox web calls Ludae needs.

use serde_json::{Value, json};
use std::time::Duration;

const UA: &str = "Ludae/0.1 (game account manager)";

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

fn answer(status: u16, what: &str) -> String {
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

/// POST with the cookie, fetching the CSRF token Roblox asks for on the first try.
fn post_signed(
    url: &str,
    cookie: &str,
    body: &Value,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let a = agent();
    let mut token = String::new();
    for _ in 0..2 {
        let mut req = a
            .post(url)
            .header("Cookie", &format!(".ROBLOSECURITY={cookie}"))
            .header("Referer", "https://www.roblox.com/")
            .header("Origin", "https://www.roblox.com");
        if !token.is_empty() {
            req = req.header("X-CSRF-TOKEN", &token);
        }
        let r = req.send_json(body).map_err(net)?;
        match r
            .headers()
            .get("x-csrf-token")
            .and_then(|t| t.to_str().ok())
        {
            Some(t) if r.status() == 403 && token.is_empty() => token = t.to_string(),
            _ => return Ok(r),
        }
    }
    Err("Roblox refused the request token".into())
}

/// (place ID, link code) of a `roblox.com/share?code=...&type=Server` link.
pub fn resolve_share_link(link: &str, cookie: &str) -> Result<(u64, String), String> {
    let code = link
        .split(['?', '&'])
        .find_map(|p| p.strip_prefix("code="))
        .ok_or("the link has no code")?;
    let mut r = post_signed(
        "https://apis.roblox.com/sharelinks/v1/resolve-link",
        cookie,
        &json!({ "linkId": code, "linkType": "Server" }),
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
/// it: "Offline", "Online", "In game: <place>", "In Studio", "Invisible".
pub fn presence(user_ids: &[u64], cookie: &str) -> Result<Vec<(u64, String)>, String> {
    let mut out = Vec::new();
    for chunk in user_ids.chunks(100) {
        let mut r = post_signed(
            "https://presence.roblox.com/v1/presence/users",
            cookie,
            &json!({ "userIds": chunk }),
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
    let r = post_signed(
        "https://auth.roblox.com/v1/authentication-ticket",
        cookie,
        &json!({}),
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
