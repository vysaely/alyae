//! Roblox on Windows: the official client, signed in with a one-time ticket. Roblox allows one
//! client at a time by holding a named mutex; while Alyae's helper holds it first, every client
//! started after it runs side by side.
//!
//! Every client shares one settings file, `%LOCALAPPDATA%\Roblox\GlobalBasicSettings_13.xml`, read
//! when the client starts. Each account keeps its own copy in Alyae's data folder: it is put in
//! place (with the account's FPS cap, and an alt's low graphics and no sound) just before that
//! account's client starts, and taken back when it closes. Launches take turns, so the new client
//! that appears belongs to the account that started it.

use crate::{Request, Role, Stop, api, fail, join_from, say, settings_xml, status};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
    PROCESS_TERMINATE, ReleaseMutex, TerminateProcess, WaitForSingleObject,
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

/// Who goes first when the PC is busy: a main runs at high priority; an alt at below normal
/// (not lower: an alt starved of CPU could lose its connection), with its memory marked low
/// priority so Windows moves an alt's memory to the page file before a main's. Many alts made
/// a main stutter at normal priority. No role: Windows' own priority.
fn set_priority(pid: u32, role: Role) {
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS, MEMORY_PRIORITY_INFORMATION,
        MEMORY_PRIORITY_LOW, PROCESS_SET_INFORMATION, ProcessMemoryPriority, SetPriorityClass,
        SetProcessInformation,
    };
    // SAFETY: plain process handle use, a correctly sized memory priority struct
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if h.is_null() {
            return;
        }
        if role == Role::Alt {
            SetPriorityClass(h, BELOW_NORMAL_PRIORITY_CLASS);
            let low = MEMORY_PRIORITY_INFORMATION {
                MemoryPriority: MEMORY_PRIORITY_LOW,
            };
            SetProcessInformation(
                h,
                ProcessMemoryPriority,
                (&raw const low).cast(),
                std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            );
        } else if role == Role::Main {
            SetPriorityClass(h, HIGH_PRIORITY_CLASS);
        }
        CloseHandle(h);
    }
}

/// Asks Windows to move the client's memory it is not using right now out to the page file. An
/// alt idling at a few frames a second keeps only a few hundred MB in RAM after it instead of
/// gigabytes, so many more fit; what it touches again comes back on its own.
fn trim_memory(pid: u32) {
    // SAFETY: plain process handle use
    unsafe {
        let h: HANDLE = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_QUOTA,
            0,
            pid,
        );
        if !h.is_null() {
            K32EmptyWorkingSet(h);
            CloseHandle(h);
        }
    }
}

/// How often an alt's memory is trimmed besides right after it loaded and when memory runs
/// short: trimming empties the whole working set, the pages the game uses as well, which it
/// then reads back from the page file. Every 30 s, 23 alts read back ~80 MB/s from a USB
/// page file (2026-11-18); Windows trims low-priority alts on its own when RAM is short.
const TRIM_EVERY: Duration = Duration::from_secs(120);

/// RAM that must be free before another client starts while others still load (room for them
/// to grow too): a client loading a big map takes gigabytes, and many loading at once froze a
/// 32 GB PC.
const FREE_TO_START_MB: u64 = 6144;
/// ... and when it would be the only one loading: one Gaia load is about 2.6 GB.
const FREE_TO_START_ALONE_MB: u64 = 3584;
/// Below this much free RAM, running alts set to trim do it at once instead of on the minute.
const LOW_MEMORY_MB: u64 = 2048;

/// Room for another client, in MB: free physical memory, but no more than what Windows can
/// still promise (commit limit, RAM plus page file) less a reserve. With the commit nearly
/// used up a new game would make running ones fail to get memory and crash.
fn start_room_mb() -> u64 {
    let (phys, commit) = memory();
    phys.min(commit.saturating_sub(COMMIT_RESERVE_MB))
}

/// Commit Windows must still be able to promise after a client starts (one Gaia alt commits
/// about 3 GB).
const COMMIT_RESERVE_MB: u64 = 4096;

/// Free physical memory, in MB.
fn free_mb() -> u64 {
    memory().0
}

/// (free physical memory, commit Windows can still promise), in MB.
fn memory() -> (u64, u64) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: a correctly sized MEMORYSTATUSEX
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) == 0 {
            return (u64::MAX, u64::MAX);
        }
        let mb = 1024 * 1024;
        (m.ullAvailPhys / mb, m.ullAvailPageFile / mb)
    }
}

/// The client's memory in RAM (its working set), in MB.
fn working_set_mb(pid: u32) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    // SAFETY: plain process handle use, a correctly sized counters struct
    unsafe {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(h, &mut c, c.cb) != 0;
        CloseHandle(h);
        ok.then_some(c.WorkingSetSize as u64 / (1024 * 1024))
    }
}

/// Touched by a launch waiting for memory: running alts set to trim do it at once when it is
/// newer than their last trim, instead of on their minute.
fn trim_now_file(req: &Request) -> PathBuf {
    req.data_dir.join("settings").join("trim_now")
}

/// Waits (holding load slot `slot`) until there is room for one more client, at most
/// `crate::MEMORY_WAIT`; asks the running alts to trim meanwhile. A stop ends the wait and the
/// helper.
fn wait_for_memory(req: &Request, slot: usize, stop: &mut Stop) {
    let ask = trim_now_file(req);
    let need = || {
        if others_loading(slot) {
            FREE_TO_START_MB
        } else {
            FREE_TO_START_ALONE_MB
        }
    };
    crate::wait_for_memory(start_room_mb, need, || {
        stop.end_if_asked();
        let _ = std::fs::write(&ask, "");
    });
}

/// Whether a load slot other than `mine` is held: another client is still loading.
fn others_loading(mine: usize) -> bool {
    use windows_sys::Win32::Foundation::{WAIT_ABANDONED_0, WAIT_OBJECT_0};
    (0..LOAD_SLOTS).filter(|k| *k != mine).any(|k| {
        // SAFETY: a named mutex, taken for an instant only to see that it is free
        unsafe {
            let h = CreateMutexW(
                std::ptr::null(),
                0,
                wide(&format!("Local\\AlyaeRobloxLoad{k}")).as_ptr(),
            );
            if h.is_null() {
                return false;
            }
            let r = WaitForSingleObject(h, 0);
            let free = r == WAIT_OBJECT_0 || r == WAIT_ABANDONED_0;
            if free {
                ReleaseMutex(h);
            }
            CloseHandle(h);
            !free
        }
    })
}

/// Whether a client whose memory was `samples` (MB, one every `LOAD_SAMPLE`) has finished
/// loading: it has had `LOAD_LEAST`, and grew less than `LOAD_STILL_MB` over the last three.
fn loaded(samples: &[u64]) -> bool {
    let least = (LOAD_LEAST.as_secs() / LOAD_SAMPLE.as_secs()) as usize;
    samples.len() > least
        && samples[samples.len() - 4..]
            .windows(2)
            .all(|w| w[1] < w[0] + LOAD_STILL_MB)
}

const LOAD_SAMPLE: Duration = Duration::from_secs(2);
const LOAD_LEAST: Duration = Duration::from_secs(8);
const LOAD_STILL_MB: u64 = 40;
/// A client still growing after this long is let be: the next starts anyway.
const LOAD_MOST: Duration = Duration::from_secs(120);

/// Waits until the new client has loaded its game (its memory stops growing), so clients load
/// one after another instead of all at once.
fn wait_until_loaded(pid: u32) {
    // "running, loading" is said by the caller
    let until = Instant::now() + LOAD_MOST;
    let mut samples = Vec::new();
    while Instant::now() < until && alive(pid) {
        match working_set_mb(pid) {
            Some(mb) => samples.push(mb),
            None => return,
        }
        if loaded(&samples) {
            return;
        }
        std::thread::sleep(LOAD_SAMPLE);
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

/// Touched when a client has just started: it reads the shared settings file in the seconds
/// after, so a launch that would change the file waits until those seconds have passed.
fn client_started_file(req: &Request) -> PathBuf {
    req.data_dir.join("settings").join("client_started")
}

/// How long a new client may still be reading the shared settings file.
const SETTINGS_READ: Duration = Duration::from_secs(5);

/// Waits until the last client started has had `SETTINGS_READ` to read the shared file.
fn wait_for_settings_read(req: &Request) {
    let since = std::fs::metadata(client_started_file(req))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());
    if let Some(left) = since.and_then(|s| SETTINGS_READ.checked_sub(s)) {
        status("waiting for the last Roblox to read its settings");
        std::thread::sleep(left);
    }
}

/// The account's settings put in place before its client starts. Only a change waits for the
/// client started before to have read the file: alts with the same settings start at once.
fn settings_before_launch(req: &Request, live: &Path) {
    let own = own_settings(req);
    if let Some(d) = own.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    settings_xml::keep_original(live, &original_settings(req), clients().is_empty());
    let own_text = own.is_file().then(|| {
        std::fs::read_to_string(&own).unwrap_or_else(|e| {
            fail(&format!(
                "cannot put this account's Roblox settings in place: {e}"
            ))
        })
    });
    let live_text = std::fs::read_to_string(live).ok();
    let mut props = Vec::new();
    if let Some(fps) = req.fps() {
        let cap = if fps == 0 || fps >= 1000 { 10000 } else { fps };
        props.push(settings_xml::prop("int", "FramerateCap", cap));
    }
    if req.role() == Role::Alt {
        props.extend(settings_xml::quality(1));
        props.push(settings_xml::prop("float", "MasterVolume", 0));
    }
    props.extend(settings_xml::window(req.window()));
    let base = own_text.or_else(|| live_text.clone());
    if base.is_some() || !props.is_empty() {
        let new = settings_xml::with_props(base.as_deref(), &props);
        if live_text.as_deref().map(str::trim_end) != Some(new.trim_end()) {
            wait_for_settings_read(req);
            let tmp = live.with_extension("alyae-tmp");
            if let Err(e) = std::fs::write(&tmp, new).and_then(|_| std::fs::rename(&tmp, live)) {
                fail(&format!("cannot write Roblox's settings: {e}"));
            }
        }
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
    let _turn = LaunchTurn::take(|| {});
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
    let window = settings_xml::window_input(req.input("window")).unwrap_or_else(|e| fail(&e));
    if props.is_empty() && window.is_none() {
        fail("nothing to change: every value is \"keep\"");
    }
    // the launch sets FramerateCap from the account's FPS cap and the window from its Window:
    // keep them in step
    if let Some(f) = fps {
        say(json!({ "set": { "fps": f.to_string() } }));
    }
    if let Some(w) = window {
        say(json!({ "set": { "window": w } }));
    }
    if props.is_empty() {
        status("window set (used from the next start)");
        return;
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

/// How many clients may load their game at the same time. Each loading client of a big map
/// takes gigabytes until it is loaded and trimmed; the free-memory check before each start
/// keeps the PC out of trouble, this keeps the start fast.
const LOAD_SLOTS: usize = 3;

/// One of `LOAD_SLOTS` named mutexes, held from a client's start until it has loaded. A
/// helper that dies leaves its mutex abandoned, which the next waiter simply takes.
struct LoadSlot(HANDLE, usize);

impl LoadSlot {
    /// Waits for a free slot; a stop meanwhile ends the helper.
    fn take(stop: &mut Stop) -> Self {
        use windows_sys::Win32::Foundation::{WAIT_ABANDONED_0, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::WaitForMultipleObjects;
        // SAFETY: named mutexes; a wait returns with exactly one of them owned
        unsafe {
            let slots: Vec<HANDLE> = (0..LOAD_SLOTS)
                .map(|i| {
                    let h = CreateMutexW(
                        std::ptr::null(),
                        0,
                        wide(&format!("Local\\AlyaeRobloxLoad{i}")).as_ptr(),
                    );
                    if h.is_null() {
                        fail("cannot create the loading lock");
                    }
                    h
                })
                .collect();
            let mut r = WaitForMultipleObjects(LOAD_SLOTS as u32, slots.as_ptr(), 0, 0);
            if r == windows_sys::Win32::Foundation::WAIT_TIMEOUT {
                status(&format!("waiting: {LOAD_SLOTS} games are loading"));
            }
            while r == windows_sys::Win32::Foundation::WAIT_TIMEOUT {
                stop.end_if_asked();
                r = WaitForMultipleObjects(LOAD_SLOTS as u32, slots.as_ptr(), 0, 250);
            }
            let i = if r >= WAIT_ABANDONED_0 {
                r - WAIT_ABANDONED_0
            } else {
                r - WAIT_OBJECT_0
            } as usize;
            for (k, h) in slots.iter().enumerate() {
                if k != i {
                    CloseHandle(*h);
                }
            }
            LoadSlot(
                slots
                    .get(i)
                    .copied()
                    .unwrap_or_else(|| fail("loading lock")),
                i,
            )
        }
    }
}

impl Drop for LoadSlot {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this thread
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Held while one account's client starts: the next launch waits for it.
struct LaunchTurn(HANDLE);

impl LaunchTurn {
    /// Waits for the turn, calling `waiting` every 250 ms meanwhile.
    fn take(mut waiting: impl FnMut()) -> Self {
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
            while WaitForSingleObject(h, 250) == windows_sys::Win32::Foundation::WAIT_TIMEOUT {
                waiting();
            }
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
    // a stop from Alyae ends the waits below, or the game once it runs
    let mut stop = Stop::listen();
    let j = join_from(req);
    // held for as long as this account's client runs: later clients find it taken and still start
    // SAFETY: named mutex creation; the handle closes when this process ends
    unsafe {
        CreateMutexW(std::ptr::null(), 1, wide("ROBLOX_singletonMutex").as_ptr());
        CreateMutexW(std::ptr::null(), 1, wide("ROBLOX_singletonEvent").as_ptr());
    }
    // up to LOAD_SLOTS clients load at once, each only with room for it; one starts at a time
    // (the turn, taken after the memory wait so an account waiting for memory does not hold up
    // one started on its own). Signing in comes after the waits, so the one-time ticket is fresh
    let slot = LoadSlot::take(&mut stop);
    // one account started on its own does not wait for free RAM; it still will not start
    // with the commit (RAM plus page file) all but used up, which would crash games
    if req.input("_alone") == "1" {
        let left = memory().1;
        if left < 1024 {
            fail(&format!(
                "{}: Windows can promise only {left} MB more (RAM plus page file). Close a game first",
                crate::NO_MEMORY
            ));
        }
    } else {
        wait_for_memory(req, slot.1, &mut stop);
    }
    let turn = LaunchTurn::take(|| stop.end_if_asked());
    stop.end_if_asked();
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
    set_priority(pid, req.role());
    // the client reads its settings while it starts: a next launch that changes them waits for
    // that (`wait_for_settings_read`), one that does not starts at once
    let _ = std::fs::write(client_started_file(req), pid.to_string());
    // the next client may start now; this one keeps its loading slot until it has loaded
    // (and, set to trim, been trimmed)
    drop(turn);
    status("running, loading");
    wait_until_loaded(pid);
    let trim = req.get("memory") == "trim";
    if trim {
        trim_memory(pid);
    }
    drop(slot);
    status("running");
    watch_client(req, pid, &live, &mut stop, trim);
}

/// The "Close all games" button: every Roblox client closes, the ones Alyae lost track of too.
/// Each account's helper then takes its settings back as after any close.
pub fn close_all() {
    let all = clients();
    for pid in &all {
        kill(*pid);
    }
    status(&format!("{} Roblox closed", all.len()));
}

/// The Resume action, when Alyae opens: if this account's client still runs from before
/// (Alyae was closed, the game was left running), watch it again as after a launch: Stop
/// works, an alt keeps being trimmed, its settings are taken back when it closes. No client:
/// ends at once, saying nothing.
pub fn resume(req: &Request) {
    let Some(pid) = std::fs::read_to_string(pid_file(req))
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        .filter(|p| alive(*p))
    else {
        return;
    };
    let live = live_settings().unwrap_or_else(|| fail("LOCALAPPDATA is not set"));
    let mut stop = Stop::listen();
    set_priority(pid, req.role());
    status("running (found again)");
    watch_client(req, pid, &live, &mut stop, req.get("memory") == "trim");
}

/// While the client runs: a stop from Alyae ends it; set to `trim`, its memory is trimmed every
/// minute, and at once whenever RAM runs low or a launch waits for memory. After it closed, its
/// settings are taken back.
fn watch_client(
    req: &Request,
    pid: u32,
    live: &Path,
    stop: &mut Stop,
    trim: bool,
) {
    let ask = trim_now_file(req);
    let mut next_trim = Instant::now() + TRIM_EVERY;
    let mut last_trim = Instant::now();
    let mut trimmed_at = SystemTime::now();
    let mut next_check = Instant::now();
    while alive(pid) {
        if stop.asked() {
            kill(pid);
            break;
        }
        let now = Instant::now();
        if trim && now >= next_check {
            next_check = now + Duration::from_secs(5);
            let asked = std::fs::metadata(&ask)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t > trimmed_at);
            let soon = now - last_trim >= Duration::from_secs(15);
            let low = free_mb() < LOW_MEMORY_MB;
            if ((low || asked) && soon) || now >= next_trim {
                trim_memory(pid);
                last_trim = now;
                trimmed_at = SystemTime::now();
                next_trim = now + TRIM_EVERY;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    settings_after_exit(req, live);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A client has loaded once it had 8 s and its memory stopped growing; still growing, or
    /// too early, it has not.
    #[test]
    fn loaded_when_memory_settles() {
        let growing: Vec<u64> = (0..10).map(|i| 300 + i * 250).collect();
        assert!(!loaded(&growing));
        let settled = [300, 900, 1500, 2100, 2500, 2580, 2600, 2610, 2615];
        assert!(loaded(&settled));
        // settled but too early
        assert!(!loaded(&[300, 300, 300, 300]));
    }
}
