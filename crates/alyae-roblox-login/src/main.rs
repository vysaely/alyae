#![cfg_attr(windows, windows_subsystem = "windows")]
//! Roblox's own login page in a window of its own, with a fresh browser profile: the username and
//! password are filled in and sent, a captcha is left to the person, and the 2FA code is filled
//! in from the saved secret when Roblox asks for it. Each step is printed as `{"status":"..."}`;
//! the `.ROBLOSECURITY` cookie Roblox then sets is printed as `{"cookie":"..."}` and the window
//! closes. Closing the window prints no cookie.
//!
//! Input (one JSON line on stdin): {"username", "password", "totp"}.
//!
//! With a `cookie` instead, the window is that account signed in on roblox.com (Open in browser):
//! the cookie goes into the fresh profile and the home page opens. On close, a cookie Roblox
//! renewed meanwhile is printed as `{"cookie":"..."}` for Alyae to keep.

use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Only pages of this origin get the saved username and password, or ask for a 2FA code.
const ORIGIN: &str = "https://www.roblox.com";

/// Fills the login form once per page load, and a 2FA box whenever one appears (asking Rust for
/// a fresh code). It runs only in Roblox's own top page: captcha frames and any other site are
/// never touched. The username and password are not in the script: Rust hands them over only
/// to the Roblox page that asks.
const SCRIPT: &str = r#"
(() => {
  if (window.top !== window || location.origin !== 'https://www.roblox.com') return;
  let askedCreds = false;
  const setValue = (el, v) => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const visible = el => el && el.offsetParent !== null;
  let sentLogin = false, askedCode = 0;
  window.__alyaeCode = code => {
    const box = twoStepBox();
    if (!box) return;
    setValue(box, code);
    const dialog = box.closest('[role="dialog"], .modal-content') || document;
    const verify = [...dialog.querySelectorAll('button')].find(b => /verify|submit/i.test(b.innerText) && !b.disabled);
    if (verify) verify.click();
  };
  const twoStepBox = () => [...document.querySelectorAll('input[inputmode="numeric"], input[maxlength="6"], input[placeholder*="code" i]')]
    .find(i => {
      if (!visible(i) || i.type === 'password' || i.id.startsWith('login-')) return false;
      const t = ((i.closest('[role="dialog"], .modal-content') || document.body).innerText || '').toLowerCase();
      return t.includes('authenticator') || t.includes('2-step') || t.includes('two-step');
    });
  const tick = () => {
    const user = document.getElementById('login-username');
    const pass = document.getElementById('login-password');
    const button = document.getElementById('login-button');
    if (!window.__alyae) {
      if (!askedCreds && visible(user)) { askedCreds = true; window.ipc.postMessage('creds'); }
      return;
    }
    if (!sentLogin && visible(user) && visible(pass) && button) {
      if (window.__alyae.username && !user.value) setValue(user, window.__alyae.username);
      if (!window.__alyae.password) { sentLogin = true; return; }
      setValue(pass, window.__alyae.password);
      if (!button.disabled) { sentLogin = true; button.click(); window.ipc.postMessage('sent'); }
    }
    // one request per Roblox code window: a rejected code waits for the next one
    if (twoStepBox() && Date.now() - askedCode > 30000) {
      askedCode = Date.now();
      window.ipc.postMessage('code');
    }
  };
  setInterval(tick, 700);
})();
"#;

enum Msg {
    Creds,
    Sent,
    Code,
}

/// A page address on Roblox's own site (the login page and where it leads).
fn is_roblox(url: &str) -> bool {
    url == ORIGIN || url.starts_with(&format!("{ORIGIN}/"))
}

/// What the window may load: Roblox and the captcha it embeds (some web views ask about frames
/// too). Anything else is refused.
fn may_open(url: &str) -> bool {
    if url == "about:blank" {
        return true;
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#', ':']).next().unwrap_or("");
    [
        "roblox.com",
        "rbxcdn.com",
        "arkoselabs.com",
        "funcaptcha.com",
        "hcaptcha.com",
    ]
    .iter()
    .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// The `.ROBLOSECURITY` cookie the web view holds now.
fn session_cookie(webview: &wry::WebView) -> Option<String> {
    webview
        .cookies_for_url(ORIGIN)
        .ok()?
        .into_iter()
        .find(|c| c.name() == ".ROBLOSECURITY")
        .map(|c| c.value().to_string())
        .filter(|v| !v.is_empty())
}

/// The web view's browser profile. WebView2 (Windows) needs a folder to write in; by default it
/// is beside the program, which a portable copy on a read-only stick or a locked-down work PC may
/// not allow. A folder in the temp directory, removed when the window closes, keeps the profile
/// fresh and the program folder untouched. WebKitGTK (Linux) needs none: the view is incognito.
struct Profile {
    #[cfg(not(target_os = "linux"))]
    context: wry::WebContext,
    #[cfg(not(target_os = "linux"))]
    dir: std::path::PathBuf,
}

impl Profile {
    fn new() -> Self {
        #[cfg(not(target_os = "linux"))]
        {
            remove_old_profiles(&std::env::temp_dir());
            let dir = std::env::temp_dir().join(format!("alyae-login-{}", std::process::id()));
            Profile {
                context: wry::WebContext::new(Some(dir.clone())),
                dir,
            }
        }
        #[cfg(target_os = "linux")]
        Profile {}
    }

    /// Removes the profile's folder (when the window has closed).
    fn remove(&self) {
        #[cfg(not(target_os = "linux"))]
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An incognito web view in `window` with `profile`, set up by `setup`.
fn web_view<'a>(
    window: &tao::window::Window,
    profile: &'a mut Profile,
    setup: impl FnOnce(WebViewBuilder<'a>) -> WebViewBuilder<'a>,
) -> wry::WebView {
    #[cfg(not(target_os = "linux"))]
    let builder = WebViewBuilder::new_with_web_context(&mut profile.context);
    #[cfg(target_os = "linux")]
    let builder = {
        let _ = profile;
        WebViewBuilder::new()
    };
    let builder = setup(builder.with_incognito(true));
    #[cfg(not(target_os = "linux"))]
    let view = builder.build(window).expect("a web view");
    #[cfg(target_os = "linux")]
    let view = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder
            .build_gtk(window.default_vbox().expect("a GTK box"))
            .expect("a web view")
    };
    view
}

/// The `.ROBLOSECURITY` cookie for every roblox.com host (www, apis, users...). wry passes
/// `Cookie::domain()` to WebKitGTK and WebView2, and the cookie crate strips one leading dot from
/// it: `.roblox.com` arrived as `roblox.com`, a cookie for that host alone, so www.roblox.com
/// opened signed out. Given `..roblox.com`, `domain()` is `.roblox.com`, a cookie for all of them.
fn session_cookie_for(value: String) -> wry::cookie::Cookie<'static> {
    wry::cookie::Cookie::build((".ROBLOSECURITY", value))
        .domain("..roblox.com")
        .path("/")
        .secure(true)
        .http_only(true)
        .build()
}

/// Roblox's site signed in as the account, from its saved cookie.
fn browse(username: &str, cookie: String) {
    let event_loop = EventLoopBuilder::new().build();
    let window = WindowBuilder::new()
        .with_title(format!("Roblox: {username}"))
        .with_inner_size(tao::dpi::LogicalSize::new(1200.0, 800.0))
        .build(&event_loop)
        .expect("a window");
    let mut profile = Profile::new();
    let webview = web_view(&window, &mut profile, |b| {
        b.with_url("about:blank")
            .with_navigation_handler(|url| may_open(&url))
    });
    let c = session_cookie_for(cookie.clone());
    if let Err(e) = webview.set_cookie(&c) {
        println!(
            "{}",
            json!({ "status": format!("cannot sign the window in: {e}") })
        );
        return;
    }
    let _ = webview.load_url(&format!("{ORIGIN}/home"));
    println!("{}", json!({ "status": "signed in on roblox.com" }));
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if let Some(now) = session_cookie(&webview).filter(|n| *n != cookie) {
                    println!("{}", json!({ "cookie": now }));
                }
                *flow = ControlFlow::Exit;
            }
            Event::LoopDestroyed => profile.remove(),
            _ => {}
        }
    });
}

fn main() {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        std::process::exit(2);
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let field = |k: &str| req[k].as_str().unwrap_or("").to_string();
    let (username, password, totp) = (field("username"), field("password"), field("totp"));
    let cookie = field("cookie");
    if !cookie.is_empty() {
        browse(&username, cookie);
        return;
    }
    let creds = json!({ "username": username, "password": password });

    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title(if username.is_empty() {
            "Sign in to Roblox".to_string()
        } else {
            format!("Sign in to Roblox: {username}")
        })
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 720.0))
        .build(&event_loop)
        .expect("a window");
    let mut profile = Profile::new();
    let webview = web_view(&window, &mut profile, |b| {
        b.with_url("https://www.roblox.com/login")
            .with_initialization_script(SCRIPT)
            // the window stays on Roblox: links elsewhere do not open in it
            .with_navigation_handler(|url| may_open(&url))
            .with_ipc_handler(move |m| {
                if !is_roblox(&m.uri().to_string()) {
                    return;
                }
                let _ = match m.body().as_str() {
                    "creds" => proxy.send_event(Msg::Creds),
                    "code" => proxy.send_event(Msg::Code),
                    "sent" => proxy.send_event(Msg::Sent),
                    _ => Ok(()),
                };
            })
    });

    let mut next_check = Instant::now();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(next_check);
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. })
            | Event::NewEvents(StartCause::Init) => {
                next_check = Instant::now() + Duration::from_millis(500);
                if let Some(c) = session_cookie(&webview) {
                    println!("{}", json!({ "cookie": c }));
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(Msg::Creds) => {
                // only into a page still on Roblox's login site
                if webview.url().is_ok_and(|u| is_roblox(&u)) {
                    let _ = webview.evaluate_script(&format!(
                        "if (location.origin === '{ORIGIN}') window.__alyae = {creds};"
                    ));
                }
            }
            Event::UserEvent(Msg::Sent) => {
                println!("{}", json!({ "status": "username and password sent" }))
            }
            Event::UserEvent(Msg::Code) => {
                if !webview.url().is_ok_and(|u| is_roblox(&u)) {
                    return;
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                match alyae_otp::code(&totp, now) {
                    Some(code) => {
                        let _ = webview
                            .evaluate_script(&format!("window.__alyaeCode({})", json!(code)));
                        println!("{}", json!({ "status": "2FA code filled in" }));
                    }
                    None => println!(
                        "{}",
                        json!({ "status": "Roblox asks for a 2FA code: none saved, type it in" })
                    ),
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *flow = ControlFlow::Exit,
            // the web view may still hold some files: what is left goes at a later login
            Event::LoopDestroyed => profile.remove(),
            _ => {}
        }
    });
}

/// Login profiles in `temp` left by windows closed more than a day ago (a folder the web view
/// still held when its window closed).
#[cfg(not(target_os = "linux"))]
fn remove_old_profiles(temp: &std::path::Path) {
    let Ok(list) = std::fs::read_dir(temp) else {
        return;
    };
    for e in list.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if old && e.file_name().to_string_lossy().starts_with("alyae-login-") {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_session_cookie_covers_every_roblox_host() {
        assert_eq!(session_cookie_for("v".into()).domain(), Some(".roblox.com"));
    }

    #[test]
    fn only_roblox_gets_the_login() {
        assert!(is_roblox("https://www.roblox.com/login"));
        assert!(!is_roblox("https://www.roblox.com.evil.io/login"));
        assert!(!is_roblox("https://evil.io/?https://www.roblox.com/"));
        assert!(may_open("https://roblox-api.arkoselabs.com/v2/x"));
        assert!(!may_open("https://notroblox.com/"));
        assert!(!may_open("http://www.roblox.com/"));
    }
}
