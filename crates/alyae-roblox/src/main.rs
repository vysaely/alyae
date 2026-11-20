//! Alyae's Roblox helper. Alyae runs `alyae-roblox <action>` and writes one JSON line: the account
//! (`account`: its fields and secrets), the button's `inputs`, a `data_dir` for this game and the
//! account's `folder` name (its number as a Roman numeral).
//! Answers are JSON lines: `{"set":{...}}`, `{"status":"..."}`, `{"error":"..."}`, `{"warn":"..."}`
//! (a line for Alyae's message bar). While a game runs the helper waits for it; a `stop` line
//! on stdin closes the game.

mod api;
mod browser;
mod files;
#[cfg(target_os = "linux")]
mod linux;
mod settings_xml;
mod tools;
#[cfg(windows)]
mod windows;

use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Request {
    pub id: String,
    /// the name of the account's own files: its number as a Roman numeral ("XII")
    folder: String,
    pub account: Map<String, Value>,
    pub inputs: Map<String, Value>,
    pub data_dir: PathBuf,
}

impl Request {
    /// The account's folder name; an account without a number has none, and ends the helper.
    pub fn folder(&self) -> &str {
        if self.folder.is_empty() {
            fail("this account has no number for its folder: restart Alyae");
        }
        &self.folder
    }
    /// The account's folder: `profiles/<numeral>`, or `profiles/<id>` while only that one is
    /// there (the name Alyae gave it before numbers, until its next launch renames it).
    pub fn profile(&self) -> PathBuf {
        let profiles = self.data_dir.join("profiles");
        files::current(&profiles.join(&self.id), &profiles.join(self.folder()))
    }
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
    pub fn role(&self) -> Role {
        match self.get("role") {
            "main" => Role::Main,
            "alt" => Role::Alt,
            _ => Role::AsIs,
        }
    }
    /// The account's Window: "fullscreen", "windowed", "small" (its default by role comes filled
    /// in from the module), or empty: Roblox's own.
    pub fn window(&self) -> &str {
        self.get("window")
    }
    /// The FPS cap Alyae sets (its default by role comes filled in from the module); None: no
    /// cap of Alyae's, 0: no cap at all.
    pub fn fps(&self) -> Option<u32> {
        self.get("fps").parse().ok()
    }
    pub fn cookie(&self) -> String {
        api::clean_cookie(self.get("cookie"))
    }
}

/// The account's Role field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    /// full FPS and quality, first in line for the CPU
    Main,
    /// small, muted and slow, last in line
    Alt,
    /// no role: Roblox runs as it is; only what the account's own fields (FPS cap, Window)
    /// ask for changes
    AsIs,
}

pub fn say(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

pub fn status(text: &str) {
    say(json!({ "status": text }));
}

/// How long a launch waits for free memory before it gives up (meanwhile running alts trim).
pub const MEMORY_WAIT: Duration = Duration::from_secs(180);

/// How a launch that gave up for memory starts its error: Alyae stops the queue on it, as the
/// accounts after would wait in vain too.
pub const NO_MEMORY: &str = "not enough free memory for another Roblox";

/// Waits until `free()` (MB) reaches `need()` MB, saying so while it waits and calling
/// `waiting()` every round (Windows asks the running alts to trim then); gives up after
/// `MEMORY_WAIT` with a `NO_MEMORY` error.
pub fn wait_for_memory(free: impl Fn() -> u64, need: impl Fn() -> u64, mut waiting: impl FnMut()) {
    let until = std::time::Instant::now() + MEMORY_WAIT;
    let mut said = (0, 0);
    loop {
        let (now, need) = (free(), need());
        if now >= need {
            return;
        }
        if std::time::Instant::now() >= until {
            fail(&format!(
                "{NO_MEMORY}: {:.1} GB free after {} min, {:.1} GB needed. Close something or start fewer accounts",
                now as f64 / 1024.0,
                MEMORY_WAIT.as_secs() / 60,
                need as f64 / 1024.0
            ));
        }
        waiting();
        // a new line only when it changed by half a GB or more
        if now.abs_diff(said.0) >= 512 || need != said.1 {
            status(&format!(
                "waiting for free memory ({:.1} GB free, {:.1} GB needed)",
                now as f64 / 1024.0,
                need as f64 / 1024.0
            ));
            said = (now, need);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
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
        } else if let Some(code) = query_value(server, "privateServerLinkCode")
            .or_else(|| query_value(server, "linkCode"))
            .filter(|c| {
                c.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            })
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
        let user = match follow.parse() {
            Ok(n) => n,
            Err(_) => api::user_id_of(follow).unwrap_or_else(|e| fail(&format!("Follow: {e}"))),
        };
        let plan = follow_plan(follow, !cfg!(windows), j.place, || {
            api::whereabouts(user, &req.cookie()).map_err(|e| format!("Follow: {e}"))
        });
        if let Some((place, said)) = plan.unwrap_or_else(|e| fail(&e)) {
            status(&said);
            j.place = Some(place);
        }
        j.follow = Some(user);
    }
    j
}

/// Follow's place, with the status line that says where it came from. Windows's
/// RequestFollowUser joins by the user alone, as a profile's Join button does, so there presence
/// is not asked (`needs_place` false: None). Mocktail wants a place beside the user (the join
/// still goes by the user, so Roblox finds their server): the place presence shows them in
/// (`look`), else their game's start place, else the one in Place ID (`typed`).
fn follow_plan(
    name: &str,
    needs_place: bool,
    typed: Option<u64>,
    look: impl FnOnce() -> Result<api::Whereabouts, String>,
) -> Result<Option<(u64, String)>, String> {
    if !needs_place {
        return Ok(None);
    }
    let seen = look()?;
    if let Some(p) = seen.place {
        return Ok(Some((p, format!("{name} is in place {p}"))));
    }
    if let Some(p) = seen.root {
        return Ok(Some((
            p,
            format!("{name}'s place is hidden: joining them through their game's start place {p}"),
        )));
    }
    match typed {
        Some(p) => Ok(Some((
            p,
            format!("{name}'s game is hidden: joining them through Place ID {p}"),
        ))),
        None => Err(format!(
            "{name}'s game is hidden from this account (offline, or their privacy hides it): \
             put their game's place ID or link in Place ID and Follow again"
        )),
    }
}

/// Log in: Roblox's login page in `alyae-roblox-login`, filled in with the saved username,
/// password and 2FA secret; the person solves a captcha if one shows. The new cookie is checked
/// and saved.
fn login(req: &Request) {
    // without a saved password the page opens with the username in, for the person to finish
    let (user, pass) = (req.get("username"), req.get("password"));
    status("login window open");
    let request = json!({ "username": user, "password": pass, "totp": req.get("totp") });
    let cookie = web_window(&request)
        .unwrap_or_else(|| fail("the login window closed before Roblox signed in"));
    match api::authenticated(&cookie) {
        Ok((id, name)) => {
            say(
                json!({ "set": { "cookie": cookie, "username": name, "user_id": id.to_string() } }),
            );
            status("signed in");
        }
        Err(e) => fail(&format!("Roblox gave a cookie that does not work: {e}")),
    }
}

/// Open in browser: roblox.com signed in as the account, in a window of its own with a fresh
/// profile. A cookie Roblox renewed while it was open is kept.
fn browser(req: &Request) {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: paste the .ROBLOSECURITY value into Cookie, or use Log in");
    }
    status("browser open");
    let request = json!({ "username": req.get("username"), "cookie": cookie });
    if let Some(renewed) = web_window(&request).filter(|c| *c != cookie)
        && api::authenticated(&renewed).is_ok()
    {
        say(json!({ "set": { "cookie": renewed } }));
        status("browser closed (renewed cookie saved)");
    }
}

/// Runs `alyae-roblox-login` with one request line, passes its steps on and returns the cookie
/// it printed, if any. The window sends the User-Agent the helper's own calls send.
fn web_window(request: &Value) -> Option<String> {
    let mut request = request.clone();
    request["user_agent"] = json!(browser::user_agent());
    let exe = std::env::current_exe()
        .ok()
        .and_then(|e| {
            e.parent().map(|d| {
                d.join(if cfg!(windows) {
                    "alyae-roblox-login.exe"
                } else {
                    "alyae-roblox-login"
                })
            })
        })
        .filter(|p| p.is_file())
        .unwrap_or_else(|| "alyae-roblox-login".into());
    let mut child = std::process::Command::new(&exe)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            fail(&format!(
                "cannot open the Roblox window ({}): {e}",
                exe.display()
            ))
        });
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{request}");
    }
    // its steps are passed on as they happen; the cookie line ends it
    let mut cookie = None;
    let mut error = None;
    if let Some(out) = child.stdout.take() {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(t) = v["status"].as_str() {
                status(t);
            }
            if let Some(c) = v["cookie"].as_str() {
                cookie = Some(c.to_string());
            }
            // Roblox refused (too many sign-ins): Alyae's queue waits longer on this text
            if let Some(e) = v["error"].as_str() {
                error = Some(e.to_string());
            }
        }
    }
    let _ = child.wait();
    if cookie.is_none()
        && let Some(e) = error
    {
        fail(&e);
    }
    cookie
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

/// Online status of the accounts: one cookie-less request per 100 user IDs, which Roblox answers
/// for everyone whose privacy shows their status publicly. An account that looks offline there
/// and was sent with its cookie is asked again as itself (an account always sees its own status),
/// about itself only, so no account's session is ever used to look up another (that would tell
/// Roblox the accounts belong together). Those own checks are spread: each account at most once
/// a minute (a quarter of them per 15 s run, an account just picked at once); one not due this run
/// shows its last own answer.
fn presence(v: &Value) {
    let accounts = v["accounts"].as_array().cloned().unwrap_or_default();
    let field = |a: &Value, k: &str| a["account"][k].as_str().unwrap_or("").trim().to_string();
    let mut all: Vec<(u64, String, String)> = Vec::new();
    for a in &accounts {
        let (Ok(user), Some(id)) = (field(a, "user_id").parse::<u64>(), a["id"].as_str()) else {
            continue;
        };
        all.push((user, id.to_string(), api::clean_cookie(&field(a, "cookie"))));
    }
    let mut map = Map::new();
    let mut errors = Vec::new();
    let ids: Vec<u64> = all.iter().map(|(u, ..)| *u).collect();
    let public = if ids.is_empty() {
        Vec::new()
    } else {
        api::presence(&ids, "").unwrap_or_else(|e| {
            errors.push(e);
            Vec::new()
        })
    };
    let turn = unix_secs() / 15 % 4;
    // the accounts the user just picked: checked now, not in their turn
    let now = v["now"].as_bool().unwrap_or(false);
    // the last own answers, so a row not due this run shows that instead of the public Offline
    let saved = v["data_dir"].as_str().map(|d| Path::new(d).join("presence-own.json"));
    let mut own: Map<String, Value> = saved
        .as_ref()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    for (i, (user, id, cookie)) in all.iter().enumerate() {
        let seen = public.iter().find(|(u, _)| u == user).map(|(_, t)| t.clone());
        let hidden = matches!(seen.as_deref(), None | Some("Offline" | "Invisible"));
        if !hidden || cookie.is_empty() {
            own.remove(id);
            if let Some(t) = seen {
                map.insert(id.clone(), json!(t));
            }
            continue;
        }
        if now || own_check_due(i, turn) {
            match api::presence(&[*user], cookie) {
                Ok(list) => {
                    if let Some((_, t)) = list.into_iter().next() {
                        own.insert(id.clone(), json!(t));
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        if let Some(t) = own.get(id).cloned().or_else(|| seen.map(Value::from)) {
            map.insert(id.clone(), t);
        }
    }
    // a full run (every account) forgets accounts that are gone; a pick run has only one
    if !now {
        own.retain(|id, _| all.iter().any(|(_, a, _)| a == id));
    }
    if let Some(f) = &saved {
        let _ = std::fs::write(f, Value::Object(own).to_string());
    }
    if !map.is_empty() || errors.is_empty() {
        say(json!({ "watch": map }));
    }
    if let Some(e) = errors.first() {
        fail(e);
    }
}

/// Whether the account at `index` gets its own check in this 15 s `turn` (0-3): each once a minute.
fn own_check_due(index: usize, turn: u64) -> bool {
    index as u64 % 4 == turn
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The Servers window. Without `of`: every place of the game of `place` (an ID or a game link,
/// the start place first), as `{"places": [{"id", "name"}]}`. With `of` (one of those): its
/// servers, `{"servers": [{"id", "players", "max", "ping", "fps"}], "wait": s}`, `wait` being
/// how long Roblox wants before the next list (3 a minute); or `{"error", "wait"}`.
fn servers(v: &Value) {
    let input = v["place"].as_str().unwrap_or("");
    let place = place_id(input).unwrap_or_else(|| fail("put a place ID or game link in Place ID"));
    let of = v["of"].as_str().and_then(|s| s.parse::<u64>().ok());
    let Some(of) = of else {
        // a game whose places Roblox does not list: the one place
        let universe = api::universe_of(place).ok();
        let mut list = universe
            .map(api::places_of)
            .and_then(Result::ok)
            .unwrap_or_default();
        if !list.iter().any(|(id, _)| *id == place) {
            list.insert(0, (place, String::new()));
        }
        let root = universe.and_then(|u| api::root_place(u).ok());
        // the start place first
        list.sort_by_key(|(id, _)| Some(*id) != root);
        let list: Vec<Value> = list
            .into_iter()
            .map(|(id, name)| json!({ "id": id.to_string(), "name": name }))
            .collect();
        let mut out = json!({ "places": list });
        if let Some(r) = root {
            out["root"] = json!(r.to_string());
        }
        say(out);
        return;
    };
    match api::servers(of) {
        Ok((list, wait)) => {
            let list: Vec<Value> = list
                .into_iter()
                .map(|s| {
                    json!({ "id": s.id, "players": s.players, "max": s.max, "ping": s.ping,
                            "fps": (s.fps * 10.0).round() / 10.0 })
                })
                .collect();
            say(json!({ "servers": list, "wait": wait }));
        }
        Err((e, wait)) => {
            say(json!({ "error": e, "wait": wait }));
            std::process::exit(1);
        }
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
    if action == "servers" {
        servers(&v);
        return;
    }
    #[cfg(target_os = "linux")]
    match action.as_str() {
        "mocktail_check" => return linux::mocktail_build::check(),
        "mocktail_build" => return linux::mocktail_build::build(&v),
        _ => {}
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
        folder: v
            .get("folder")
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
    if !req.folder.chars().all(|c| "IVXLCDM".contains(c)) {
        fail("bad folder name");
    }
    match action.as_str() {
        "check" => check(&req),
        "login" => login(&req),
        "browser" => browser(&req),
        #[cfg(target_os = "linux")]
        "graphics" => linux::graphics(&req),
        #[cfg(windows)]
        "graphics" => windows::graphics(&req),
        #[cfg(target_os = "linux")]
        "reset" => linux::reset(&req),
        #[cfg(windows)]
        "reset" => windows::reset(&req),
        #[cfg(target_os = "linux")]
        "forget" => linux::forget(&req),
        #[cfg(target_os = "linux")]
        "close_all" => linux::close_all(),
        #[cfg(target_os = "linux")]
        "anti_afk" => linux::anti_afk(),
        #[cfg(windows)]
        "close_all" => windows::close_all(),
        #[cfg(target_os = "linux")]
        "resume" => linux::resume(&req),
        #[cfg(windows)]
        "resume" => windows::resume(&req),
        "signout_all" | "logout" | "change_password" | "unblock_all" | "friends" | "privacy" => {
            tools::run(&action, &req)
        }
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

/// Alyae's Stop for one game, listened for from before a launch's waits until the game ends.
pub struct Stop {
    rx: std::sync::mpsc::Receiver<bool>,
    /// Alyae closed (its end of stdin did): nobody reads this helper's answers any more
    pub alyae_gone: bool,
}

impl Stop {
    pub fn listen() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(wait_for_stop());
        });
        Stop {
            rx,
            alyae_gone: false,
        }
    }

    /// Whether Alyae asked to stop since the last call.
    pub fn asked(&mut self) -> bool {
        match self.rx.try_recv() {
            Ok(true) => true,
            Ok(false) => {
                self.alyae_gone = true;
                false
            }
            Err(_) => false,
        }
    }

    /// For the waits before the game starts: a stop ends the helper there, nothing started.
    pub fn end_if_asked(&mut self) {
        if self.asked() {
            status("stopped before starting");
            std::process::exit(0);
        }
    }
}

/// Lines from Alyae after the request: true when it asked to stop (false when Alyae closed).
fn wait_for_stop() -> bool {
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
        assert_eq!(place_id("1818"), Some(1818));
        assert_eq!(
            place_id("https://www.roblox.com/games/1818/Classic-Crossroads"),
            Some(1818)
        );
        assert_eq!(place_id("crossroads"), None);
        let link = "https://www.roblox.com/games/123/x?privateServerLinkCode=abcDEF123456789012";
        assert_eq!(
            query_value(link, "privateServerLinkCode"),
            Some("abcDEF123456789012")
        );
        assert_eq!(query_value(link, "linkCode"), None);
    }

    #[test]
    fn follow_on_windows_asks_no_presence() {
        let plan = follow_plan("Bob", false, Some(5), || panic!("presence asked"));
        assert_eq!(plan, Ok(None));
    }

    #[test]
    fn follow_place_order() {
        let seen = |place, root| move || Ok(api::Whereabouts { place, root });
        let plan = |place, root, typed| follow_plan("Bob", true, typed, seen(place, root));
        // the place presence shows comes first, then the game's start place, then Place ID
        assert_eq!(
            plan(Some(10), Some(20), Some(30)),
            Ok(Some((10, "Bob is in place 10".into())))
        );
        assert_eq!(
            plan(None, Some(20), Some(30)),
            Ok(Some((
                20,
                "Bob's place is hidden: joining them through their game's start place 20".into()
            )))
        );
        assert_eq!(
            plan(None, None, Some(30)),
            Ok(Some((
                30,
                "Bob's game is hidden: joining them through Place ID 30".into()
            )))
        );
        assert!(plan(None, None, None).unwrap_err().contains("Place ID"));
        // a presence that failed is said as it is, not taken for hidden
        assert_eq!(
            follow_plan("Bob", true, Some(30), || Err("Follow: offline".into())),
            Err("Follow: offline".into())
        );
    }

    #[test]
    fn own_checks_once_a_minute_each() {
        for i in 0..47 {
            let turns = (0..4).filter(|&t| own_check_due(i, t)).count();
            assert_eq!(turns, 1, "account {i}");
        }
        assert_eq!((0..47).filter(|&i| own_check_due(i, 2)).count(), 12);
    }
}
