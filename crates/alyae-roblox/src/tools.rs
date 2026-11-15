//! The account tools (AEAM's batch tools): sign out everywhere, log out, change password, unblock
//! everyone, friend requests and friends, who can join. Each runs for one account; Alyae runs it
//! for every chosen account, and `index` (the account's place in that list) spaces them out so
//! Roblox does not see a burst.

use crate::api::{self, Session};
use crate::{Request, fail, say, status};
use serde_json::{Value, json};
use std::time::Duration;

/// Seconds between two accounts of one batch (AEAM used 3.5).
const PACE: u64 = 4;

fn session(req: &Request) -> Session {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: sign this account in first");
    }
    // the device ID Mocktail uses for this account, else a new one for this run
    let tracker = std::fs::read_to_string(
        req.data_dir
            .join("profiles")
            .join(&req.id)
            .join("browser_tracker"),
    )
    .ok()
    .or_else(|| {
        let p = req.get("profile");
        (!p.is_empty()).then(|| {
            std::fs::read_to_string(std::path::Path::new(p).join("browser_tracker")).ok()
        })?
    })
    .map(|t| t.trim().to_string())
    .filter(|t| api::browser_id(t).is_some())
    .or_else(api::new_browser_tracker);
    Session {
        cookie,
        tracker,
        totp: req.get("totp").to_string(),
    }
}

fn ok_or_fail(r: &mut api::Response, what: &str) -> Value {
    let code = r.status().as_u16();
    let body: Value = r.body_mut().read_json().unwrap_or(Value::Null);
    if !(200..300).contains(&code) {
        let msg = body["errors"][0]["message"]
            .as_str()
            .map(String::from)
            .unwrap_or_else(|| api::answer(code, what));
        fail(&format!("{what}: {msg}"));
    }
    body
}

fn post(url: &str, s: &Session, body: Value, what: &str) -> (Value, Option<String>) {
    let mut r = api::call(url, s, Some(&body)).unwrap_or_else(|e| fail(&e));
    let cookie = api::new_cookie(&r);
    (ok_or_fail(&mut r, what), cookie)
}

fn get(url: &str, s: &Session, what: &str) -> Value {
    let mut r = api::call(url, s, None).unwrap_or_else(|e| fail(&e));
    ok_or_fail(&mut r, what)
}

/// A password Roblox accepts: 20 letters and digits from the system's random source.
fn new_password() -> String {
    const ABC: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 20];
    getrandom::fill(&mut bytes).unwrap_or_else(|e| fail(&format!("no random source: {e}")));
    bytes
        .iter()
        .map(|b| ABC[*b as usize % ABC.len()] as char)
        .collect()
}

pub fn run(action: &str, req: &Request) {
    let index: u64 = req.input("index").parse().unwrap_or(0);
    if index > 0 {
        status(&format!("waiting {}s", index * PACE));
        std::thread::sleep(Duration::from_secs(index * PACE));
    }
    let s = session(req);
    match action {
        "signout_all" => {
            let (_, cookie) = post(
                "https://auth.roblox.com/v1/logoutfromallsessionsandreauthenticate",
                &s,
                json!({}),
                "Sign out everywhere",
            );
            match cookie {
                Some(c) => say(json!({ "set": { "cookie": c } })),
                None => {
                    fail("Roblox signed the other sessions out but sent no new cookie for this one")
                }
            }
            status("signed out everywhere else");
        }
        "logout" => {
            post(
                "https://auth.roblox.com/v2/logout",
                &s,
                json!({}),
                "Log out",
            );
            // the cookie is no longer valid: keep none
            say(json!({ "set": { "cookie": "" } }));
            status("logged out");
        }
        "change_password" => {
            let current = req.get("password");
            if current.is_empty() {
                fail("the account's current password is not saved");
            }
            let new = match req.input("new_password") {
                "" => new_password(),
                p => p.to_string(),
            };
            let (_, cookie) = post(
                "https://auth.roblox.com/v2/user/passwords/change",
                &s,
                json!({ "currentPassword": current, "newPassword": new }),
                "Change password",
            );
            // Roblox ends the other sessions and signs this one in again
            let mut set = json!({ "password": new });
            if let Some(c) = cookie {
                set["cookie"] = json!(c);
            }
            say(json!({ "set": set }));
            status("password changed");
        }
        "unblock_all" => {
            let mut ids = Vec::new();
            let mut cursor = String::new();
            loop {
                let mut url =
                    "https://apis.roblox.com/user-blocking-api/v1/users/get-blocked-users?count=50"
                        .to_string();
                if !cursor.is_empty() {
                    url += &format!("&cursor={cursor}");
                }
                let v = get(&url, &s, "Blocked list");
                ids.extend(
                    v["data"]["blockedUsers"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|u| u["blockedUserId"].as_u64()),
                );
                match v["data"]["cursor"].as_str() {
                    Some(c) if !c.is_empty() => cursor = c.to_string(),
                    _ => break,
                }
            }
            for (n, id) in ids.iter().enumerate() {
                status(&format!("unblocking {}/{}", n + 1, ids.len()));
                post(
                    &format!(
                        "https://apis.roblox.com/user-blocking-api/v1/users/{id}/unblock-user"
                    ),
                    &s,
                    json!({}),
                    "Unblock",
                );
            }
            status(&format!("unblocked {}", ids.len()));
        }
        "friends" => match req.input("what") {
            "Decline all requests" => {
                post(
                    "https://friends.roblox.com/v1/user/friend-requests/decline-all",
                    &s,
                    json!({}),
                    "Decline requests",
                );
                status("requests declined");
            }
            "Accept all requests" => {
                let v = get(
                    "https://friends.roblox.com/v1/my/friends/requests?sortOrder=Desc&limit=100",
                    &s,
                    "Friend requests",
                );
                let ids: Vec<u64> = v["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|u| u["id"].as_u64())
                    .collect();
                for id in &ids {
                    post(
                        &format!("https://friends.roblox.com/v1/users/{id}/accept-friend-request"),
                        &s,
                        json!({}),
                        "Accept",
                    );
                }
                status(&format!("accepted {}", ids.len()));
            }
            "Remove all friends" => {
                let user = req.get("user_id");
                if user.is_empty() {
                    fail("no user ID: Refresh this account first");
                }
                let v = get(
                    &format!("https://friends.roblox.com/v1/users/{user}/friends"),
                    &s,
                    "Friends",
                );
                let ids: Vec<u64> = v["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|u| u["id"].as_u64())
                    .collect();
                for id in &ids {
                    post(
                        &format!("https://friends.roblox.com/v1/users/{id}/unfriend"),
                        &s,
                        json!({}),
                        "Unfriend",
                    );
                }
                status(&format!("removed {}", ids.len()));
            }
            other => fail(&format!("unknown choice '{other}'")),
        },
        "privacy" => {
            let who = req.input("who");
            post(
                "https://apis.roblox.com/user-settings-api/v1/user-settings",
                &s,
                json!({ "whoCanJoinMeInExperiences": who }),
                "Who can join",
            );
            status(&format!("who can join: {who}"));
        }
        _ => unreachable!("main.rs sends only these"),
    }
}
