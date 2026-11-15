//! Roblox on Windows: the official client, signed in with a one-time ticket. Roblox allows one
//! client at a time by holding a named mutex; while Ludae's helper holds it first, every client
//! started after it runs side by side.

use crate::{Request, api, fail, join_from, status, wait_for_stop};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenProcess, PROCESS_TERMINATE, TerminateProcess,
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

pub fn play(req: &Request) {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: paste the .ROBLOSECURITY value into Cookie");
    }
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
    let launcher = match (j.follow, j.place) {
        (Some(user), _) => Some(format!(
            "{base}?request=RequestFollowUser&browserTrackerId={browser}&userId={user}"
        )),
        (None, Some(p)) => Some(match (&j.job, &j.link_code) {
            (Some(job), _) => format!(
                "{base}?request=RequestGameJob&browserTrackerId={browser}&placeId={p}&gameId={job}&isPlayTogetherGame=false"
            ),
            (None, Some(code)) => format!(
                "{base}?request=RequestPrivateGame&browserTrackerId={browser}&placeId={p}&linkCode={code}"
            ),
            (None, None) => format!(
                "{base}?request=RequestGame&browserTrackerId={browser}&placeId={p}&isPlayTogetherGame=false"
            ),
        }),
        (None, None) => None,
    };
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
}
