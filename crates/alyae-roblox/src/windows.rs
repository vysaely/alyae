//! Roblox on Windows: the official client, signed in with a one-time ticket. Roblox allows one
//! client at a time by holding a named mutex; while Alyae's helper holds it first, every client
//! started after it runs side by side.
//!
//! Every client shares one settings file, `%LOCALAPPDATA%\Roblox\GlobalBasicSettings_13.xml`, read
//! when the client starts. Each account keeps its own copy in Alyae's data folder: it is put in
//! place (with the account's FPS cap, and an alt's low graphics and no sound) just before that
//! account's client starts, and taken back when it closes. Launches take turns, so the new client
//! that appears belongs to the account that started it.

use crate::{Request, api, fail, join_from, say, settings_xml, status, wait_for_stop};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, INFINITE, OpenProcess, PROCESS_TERMINATE, ReleaseMutex, TerminateProcess,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Process IDs of running Roblox clients.
fn clients() -> Vec<u32> {
    let mut out = Vec::new();
    // SAFETY: Toolhelp snapshot walk with a correctly sized PROCESSENTRY32W
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            let len = e
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(e.szExeFile.len());
            let name = String::from_utf16_lossy(&e.szExeFile[..len]);
            if name.eq_ignore_ascii_case("RobloxPlayerBeta.exe") {
                out.push(e.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

fn alive(pid: u32) -> bool {
    clients().contains(&pid)
}

fn kill(pid: u32) {
    // SAFETY: plain process handle use
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !h.is_null() {
            TerminateProcess(h, 0);
            CloseHandle(h);
        }
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Roblox's settings file, shared by every client.
fn live_settings() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(
        Path::new(&base)
            .join("Roblox")
            .join("GlobalBasicSettings_13.xml"),
    )
}

/// This account's own copy of the settings file.
fn own_settings(req: &Request) -> PathBuf {
    req.data_dir
        .join("settings")
        .join(format!("{}.xml", req.id))
}

/// The person's own settings file, kept while Alyae's accounts use the shared one.
fn original_settings(req: &Request) -> PathBuf {
    req.data_dir.join("settings").join("original.xml")
}

/// Who launched last: only that account's settings are still in the shared file at its exit.
fn last_launch_file(req: &Request) -> PathBuf {
    req.data_dir.join("settings").join("last_launch")
}

/// The account's settings put in place before its client starts.
fn settings_before_launch(req: &Request, live: &Path) {
    let own = own_settings(req);
    if let Some(d) = own.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    settings_xml::keep_original(live, &original_settings(req), clients().is_empty());
    if own.is_file()
        && let Err(e) = std::fs::copy(&own, live)
    {
        fail(&format!(
            "cannot put this account's Roblox settings in place: {e}"
        ));
    }
    let mut props = Vec::new();
    if let Some(fps) = req.fps() {
        let cap = if fps == 0 || fps >= 1000 { 10000 } else { fps };
        props.push(settings_xml::prop("int", "FramerateCap", cap));
    }
    if req.is_alt() {
        props.extend(settings_xml::quality(1));
        props.push(settings_xml::prop("float", "MasterVolume", 0));
    }
    if !props.is_empty()
        && let Err(e) = settings_xml::apply(live, &props)
    {
        fail(&format!("cannot write Roblox's settings: {e}"));
    }
    let _ = std::fs::write(last_launch_file(req), &req.id);
}

/// After the client closed: the shared file goes back to the account's copy, unless another
/// account launched since (the file then holds that account's settings).
fn settings_after_exit(req: &Request, live: &Path) {
    let last = std::fs::read_to_string(last_launch_file(req)).unwrap_or_default();
    if last.trim() == req.id {
        let _ = std::fs::copy(live, own_settings(req));
    }
    // the last client gone: the person's own settings come back (in the launch turn, so a
    // client starting right now is not handed them)
    let _turn = LaunchTurn::take();
    settings_xml::give_back_original(live, &original_settings(req), clients().is_empty());
}

/// Where the account's running client is noted (its process ID), for Settings to see.
fn pid_file(req: &Request) -> PathBuf {
    req.data_dir
        .join("settings")
        .join(format!("{}.pid", req.id))
}

fn running(req: &Request) -> bool {
    std::fs::read_to_string(pid_file(req))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .is_some_and(alive)
}

/// The Settings form into the account's own copy, used from its next start. Its client must be
/// closed: the copy is taken back from Roblox's file when the client closes.
pub fn graphics(req: &Request) {
    if running(req) {
        fail("close this account's Roblox first: its settings are taken back when it closes");
    }
    let (props, fps) =
        settings_xml::from_form(|k| req.input(k).to_string()).unwrap_or_else(|e| fail(&e));
    if props.is_empty() {
        fail("nothing to change: every value is \"keep\"");
    }
    // the launch sets FramerateCap from the account's FPS cap: keep the two in step
    if let Some(f) = fps {
        say(json!({ "set": { "fps": f.to_string() } }));
    }
    let own = own_settings(req);
    if let Some(d) = own.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // the first time: start from the settings Roblox has now
    if !own.is_file()
        && let Some(live) = live_settings().filter(|l| l.is_file())
    {
        let _ = std::fs::copy(live, &own);
    }
    match settings_xml::apply(&own, &props) {
        Ok(()) => status(&format!(
            "{} settings set (used from the next start)",
            props.len()
        )),
        Err(e) => fail(&format!("cannot write the settings: {e}")),
    }
}

/// Held while one account's client starts: the next launch waits for it.
struct LaunchTurn(HANDLE);

impl LaunchTurn {
    fn take() -> Self {
        // SAFETY: named mutex; the wait returns when this process owns it (or its owner died)
        unsafe {
            let h = CreateMutexW(
                std::ptr::null(),
                0,
                wide("Local\\AlyaeRobloxLaunch").as_ptr(),
            );
            if h.is_null() {
                fail("cannot create the launch lock");
            }
            WaitForSingleObject(h, INFINITE);
            LaunchTurn(h)
        }
    }
}

impl Drop for LaunchTurn {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this thread
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

pub fn play(req: &Request) {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: paste the .ROBLOSECURITY value into Cookie");
    }
    let live = live_settings().unwrap_or_else(|| fail("LOCALAPPDATA is not set"));
    let j = join_from(req);
    // held for as long as this account's client runs: later clients find it taken and still start
    // SAFETY: named mutex creation; the handle closes when this process ends
    unsafe {
        CreateMutexW(std::ptr::null(), 1, wide("ROBLOX_singletonMutex").as_ptr());
        CreateMutexW(std::ptr::null(), 1, wide("ROBLOX_singletonEvent").as_ptr());
    }
    status("signing in");
    let ticket = api::auth_ticket(&cookie).unwrap_or_else(|e| fail(&e));
    let tracker = api::new_browser_tracker();
    let browser = tracker
        .as_deref()
        .and_then(api::browser_id)
        .unwrap_or("0")
        .to_string();
    let base = "https://www.roblox.com/Game/PlaceLauncher.ashx";
    // a follow joins as Roblox's own friend Join does: by user, so Roblox places the account in
    // that user's server even when it is a sub-place or a game-made (reserved) server, which a
    // join by job ID is refused ("restricted")
    let follow = j
        .follow
        .map(|u| format!("{base}?request=RequestFollowUser&browserTrackerId={browser}&userId={u}"));
    let launcher = follow.or_else(|| j.place.map(|p| match (&j.job, &j.link_code) {
        (Some(job), _) => format!(
            "{base}?request=RequestGameJob&browserTrackerId={browser}&placeId={p}&gameId={job}&isPlayTogetherGame=false"
        ),
        (None, Some(code)) => format!(
            "{base}?request=RequestPrivateGame&browserTrackerId={browser}&placeId={p}&linkCode={code}"
        ),
        (None, None) => format!(
            "{base}?request=RequestGame&browserTrackerId={browser}&placeId={p}&isPlayTogetherGame=false"
        ),
    }));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut url = format!(
        "roblox-player:1+launchmode:{}+gameinfo:{ticket}+launchtime:{now}",
        if launcher.is_some() { "play" } else { "app" }
    );
    if let Some(l) = launcher {
        url += &format!("+placelauncherurl:{}", enc(&l));
    }
    url += &format!(
        "+browsertrackerid:{browser}+robloxLocale:en_us+gameLocale:en_us+channel:+LaunchExp:InApp"
    );
    let turn = LaunchTurn::take();
    settings_before_launch(req, &live);
    let before = clients();
    // SAFETY: ShellExecuteW with NUL-terminated wide strings
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            wide(&url).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (r as isize) <= 32 {
        fail("Roblox is not installed (no roblox-player: handler)");
    }
    status("starting");
    let until = Instant::now() + Duration::from_secs(90);
    let pid = loop {
        if let Some(p) = clients().into_iter().find(|p| !before.contains(p)) {
            break p;
        }
        if Instant::now() > until {
            fail("the Roblox client did not start");
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let _ = std::fs::write(pid_file(req), pid.to_string());
    // the client reads its settings while it starts: the next launch waits a little longer
    std::thread::sleep(Duration::from_secs(5));
    drop(turn);
    status("running");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if wait_for_stop() {
            let _ = tx.send(());
        }
    });
    while alive(pid) {
        if rx.try_recv().is_ok() {
            kill(pid);
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    settings_after_exit(req, &live);
    let _ = std::fs::remove_file(pid_file(req));
}

/// The Reset files button. Every client on Windows shares Roblox's files, so this is for all
/// accounts at once and only while no client runs: the downloaded-asset cache (`%TEMP%\Roblox`)
/// and the logs (`%LOCALAPPDATA%\Roblox\logs`) go and are made again on the next launch. Kept:
/// the installed client, the settings file, each account's own settings in Alyae's folder.
pub fn reset(_req: &Request) {
    if !clients().is_empty() {
        fail("close every Roblox first: on Windows the accounts share these files");
    }
    let mut freed = 0;
    if let Some(t) = std::env::var_os("TEMP") {
        freed += crate::files::remove(&Path::new(&t).join("Roblox"));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        freed += crate::files::remove(&Path::new(&l).join("Roblox").join("logs"));
    }
    status(&format!("files reset, {} MB freed", freed / 1_000_000));
}
