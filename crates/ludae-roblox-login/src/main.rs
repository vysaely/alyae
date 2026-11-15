#![cfg_attr(windows, windows_subsystem = "windows")]
//! Roblox's own login page in a window of its own, with a fresh browser profile: the username and
//! password are filled in and sent, a captcha is left to the person, and the 2FA code is filled
//! in from the saved secret when Roblox asks for it. Each step is printed as `{"status":"..."}`;
//! the `.ROBLOSECURITY` cookie Roblox then sets is printed as `{"cookie":"..."}` and the window
//! closes. Closing the window prints no cookie.
//!
//! Input (one JSON line on stdin): {"username", "password", "totp"}.

use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

/// Fills the login form once per page load, and a 2FA box whenever one appears (asking Rust for
/// a fresh code). Captcha frames are never touched.
const SCRIPT: &str = r#"
(() => {
  const setValue = (el, v) => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  };
  const visible = el => el && el.offsetParent !== null;
  let sentLogin = false, askedCode = 0;
  window.__ludaeCode = code => {
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
    if (!sentLogin && visible(user) && visible(pass) && button) {
      setValue(user, window.__ludae.username);
      setValue(pass, window.__ludae.password);
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
    Sent,
    Code,
}

fn main() {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        std::process::exit(2);
    }
    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let field = |k: &str| req[k].as_str().unwrap_or("").to_string();
    let (username, password, totp) = (field("username"), field("password"), field("totp"));
    let creds = json!({ "username": username, "password": password });

    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title(format!("Sign in to Roblox: {username}"))
        .with_inner_size(tao::dpi::LogicalSize::new(480.0, 720.0))
        .build(&event_loop)
        .expect("a window");
    let builder = WebViewBuilder::new()
        .with_url("https://www.roblox.com/login")
        .with_incognito(true)
        .with_initialization_script(&format!("window.__ludae = {creds};\n{SCRIPT}"))
        .with_ipc_handler(move |m| {
            let _ = match m.body().as_str() {
                "code" => proxy.send_event(Msg::Code),
                "sent" => proxy.send_event(Msg::Sent),
                _ => Ok(()),
            };
        });
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window).expect("a web view");
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder
            .build_gtk(window.default_vbox().expect("a GTK box"))
            .expect("a web view")
    };

    let mut next_check = Instant::now();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(next_check);
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. })
            | Event::NewEvents(StartCause::Init) => {
                next_check = Instant::now() + Duration::from_millis(500);
                let cookie = webview
                    .cookies_for_url("https://www.roblox.com")
                    .ok()
                    .and_then(|list| list.into_iter().find(|c| c.name() == ".ROBLOSECURITY"))
                    .map(|c| c.value().to_string())
                    .filter(|v| !v.is_empty());
                if let Some(c) = cookie {
                    println!("{}", json!({ "cookie": c }));
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(Msg::Sent) => {
                println!("{}", json!({ "status": "username and password sent" }))
            }
            Event::UserEvent(Msg::Code) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                match ludae_otp::code(&totp, now) {
                    Some(code) => {
                        let _ = webview
                            .evaluate_script(&format!("window.__ludaeCode({})", json!(code)));
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
            _ => {}
        }
    });
}
