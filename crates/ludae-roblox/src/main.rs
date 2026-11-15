//! Ludae's Roblox helper. Ludae runs `ludae-roblox <action>` and writes one JSON line: the account
//! (`account`: its fields and secrets), the button's `inputs` and a `data_dir` for this game.
//! Answers are JSON lines: `{"set":{...}}`, `{"status":"..."}`, `{"error":"..."}`. While a game
//! runs the helper waits for it; a `stop` line on stdin closes the game.

mod api;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub struct Request {
    pub id: String,
    pub account: Map<String, Value>,
    pub inputs: Map<String, Value>,
    pub data_dir: PathBuf,
}

impl Request {
    pub fn get(&self, key: &str) -> &str {
        self.account
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
    }
    pub fn input(&self, key: &str) -> &str {
        self.inputs
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
    }
    pub fn is_main(&self) -> bool {
        self.get("role") == "main"
    }
    /// 0 means no cap
    pub fn fps(&self) -> u32 {
        self.get("fps")
            .parse()
            .unwrap_or(if self.is_main() { 330 } else { 6 })
    }
    pub fn cookie(&self) -> String {
        api::clean_cookie(self.get("cookie"))
    }
}

pub fn say(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

pub fn status(text: &str) {
    say(json!({ "status": text }));
}

pub fn fail(text: &str) -> ! {
    say(json!({ "error": text }));
    std::process::exit(1);
}

/// Where to join, from the Play inputs.
pub struct Join {
    pub place: Option<u64>,
    pub job: Option<String>,
    pub link_code: Option<String>,
    pub follow: Option<u64>,
}

/// The first run of digits after `/games/` in a link, or the whole input when it is a number.
pub fn place_id(input: &str) -> Option<u64> {
    let s = input.trim();
    if let Ok(n) = s.parse() {
        return Some(n);
    }
    let after = s
        .split("/games/")
        .nth(1)
        .or_else(|| s.split("placeId=").nth(1))?;
    after
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn query_value<'a>(link: &'a str, key: &str) -> Option<&'a str> {
    link.split(['?', '&'])
        .find_map(|p| p.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .filter(|v| !v.is_empty())
}

pub fn join_from(req: &Request) -> Join {
    let mut j = Join {
        place: place_id(req.input("place")),
        job: None,
        link_code: None,
        follow: None,
    };
    let server = req.input("server");
    if !server.is_empty() {
        let is_job =
            server.len() == 36 && server.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        if is_job {
            j.job = Some(server.to_string());
        } else if let Some(code) =
            query_value(server, "privateServerLinkCode").or_else(|| query_value(server, "linkCode"))
        {
            j.link_code = Some(code.to_string());
            j.place = j.place.or_else(|| place_id(server));
        } else if server.contains("share?code=") || server.contains("/share") {
            match api::resolve_share_link(server, &req.cookie()) {
                Ok((place, code)) => {
                    j.place = j.place.or(Some(place));
                    j.link_code = Some(code);
                }
                Err(e) => fail(&format!("private server link: {e}")),
            }
        } else if server
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && server.len() > 15
        {
            j.link_code = Some(server.to_string());
        } else {
            fail("Server: a job ID or a private server link");
        }
    }
    let follow = req.input("follow");
    if !follow.is_empty() {
        j.follow = Some(match follow.parse() {
            Ok(n) => n,
            Err(_) => api::user_id_of(follow).unwrap_or_else(|e| fail(&format!("Follow: {e}"))),
        });
    }
    j
}

/// Checks the cookie and fills in the name and user ID.
fn check(req: &Request) {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: paste the .ROBLOSECURITY value into Cookie");
    }
    match api::authenticated(&cookie) {
        Ok((id, name)) => {
            say(json!({ "set": { "username": name, "user_id": id.to_string() } }));
            status("signed in");
        }
        Err(e) => fail(&e),
    }
}

/// Online status of all accounts: one request per 100, signed with the first account's cookie
/// (Roblox shows presence its privacy settings hide only to a signed-in viewer).
fn presence(v: &Value) {
    let accounts = v["accounts"].as_array().cloned().unwrap_or_default();
    let field = |a: &Value, k: &str| a["account"][k].as_str().unwrap_or("").trim().to_string();
    let by_user: Vec<(u64, String)> = accounts
        .iter()
        .filter_map(|a| {
            Some((
                field(a, "user_id").parse().ok()?,
                a["id"].as_str()?.to_string(),
            ))
        })
        .collect();
    let cookie = accounts
        .iter()
        .map(|a| api::clean_cookie(&field(a, "cookie")))
        .find(|c| !c.is_empty())
        .unwrap_or_default();
    let ids: Vec<u64> = by_user.iter().map(|(u, _)| *u).collect();
    match api::presence(&ids, &cookie) {
        Ok(list) => {
            let mut map = Map::new();
            for (user, text) in list {
                for (_, id) in by_user.iter().filter(|(u, _)| *u == user) {
                    map.insert(id.clone(), json!(text));
                }
            }
            say(json!({ "watch": map }));
        }
        Err(e) => fail(&e),
    }
}

fn main() {
    let action = std::env::args().nth(1).unwrap_or_default();
    let mut line = String::new();
    let mut stdin = std::io::stdin().lock();
    if stdin.read_line(&mut line).is_err() {
        fail("no request on stdin");
    }
    drop(stdin);
    let v: Value =
        serde_json::from_str(&line).unwrap_or_else(|e| fail(&format!("bad request: {e}")));
    if action == "presence" {
        presence(&v);
        return;
    }
    let obj = |k: &str| {
        v.get(k)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let req = Request {
        id: v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        account: obj("account"),
        inputs: obj("inputs"),
        data_dir: PathBuf::from(v.get("data_dir").and_then(Value::as_str).unwrap_or(".")),
    };
    if req.id.is_empty()
        || !req
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        fail("bad account id");
    }
    match action.as_str() {
        "check" => check(&req),
        "play" | "follow" => {
            #[cfg(target_os = "linux")]
            linux::play(&req);
            #[cfg(windows)]
            windows::play(&req);
            #[cfg(not(any(target_os = "linux", windows)))]
            fail("Roblox needs Linux (Mocktail) or Windows");
        }
        other => fail(&format!("unknown action '{other}'")),
    }
}

/// Lines from Ludae after the request: true when it asked to stop (false when Ludae closed).
pub fn wait_for_stop() -> bool {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        match line {
            Ok(l) if l.trim() == "stop" => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_and_codes() {
        assert_eq!(place_id("3016661674"), Some(3016661674));
        assert_eq!(
            place_id("https://www.roblox.com/games/3016661674/Rogue-Lineage"),
            Some(3016661674)
        );
        assert_eq!(place_id("rogue"), None);
        let link = "https://www.roblox.com/games/123/x?privateServerLinkCode=abcDEF123456789012";
        assert_eq!(
            query_value(link, "privateServerLinkCode"),
            Some("abcDEF123456789012")
        );
        assert_eq!(query_value(link, "linkCode"), None);
    }
}
