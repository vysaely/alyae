//! Roblox on Linux through Mocktail: one profile folder per account, a per-account copy of the
//! Mocktail program (its single-instance lock follows the program's path), a main or alt role.

use crate::files::{clear, remove, rename_old};
use crate::{Join, Request, Role, Stop, api, fail, join_from, say, settings_xml, status};
use serde_json::json;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub mod mocktail_build;

struct Mocktail {
    /// the real program
    bin: PathBuf,
    /// its companion libraries (libmocktail_audio_sdl.so ...)
    lib: PathBuf,
    /// shared data: payloads (the Roblox client itself), metadata
    data: PathBuf,
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

fn find_mocktail() -> Option<Mocktail> {
    let h = home();
    let lib_dirs = [
        h.join(".local/lib/mocktail"),
        "/usr/lib/mocktail".into(),
        "/usr/local/lib/mocktail".into(),
        "/opt/mocktail/lib".into(),
    ];
    let mut bins: Vec<PathBuf> = std::env::var_os("ALYAE_MOCKTAIL")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    bins.extend(lib_dirs.iter().map(|d| d.join("mocktail")));
    bins.extend([
        h.join(".local/bin/mocktail"),
        "/usr/bin/mocktail".into(),
        "/usr/local/bin/mocktail".into(),
    ]);
    let bin = bins.into_iter().find(|b| b.is_file())?;
    let bin = fs::canonicalize(&bin).unwrap_or(bin);
    let lib = std::iter::once(bin.parent()?.to_path_buf())
        .chain(lib_dirs)
        .find(|d| d.join("libmocktail_audio_sdl.so").exists())?;
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| h.join(".local/share"));
    Some(Mocktail {
        bin,
        lib,
        data: data_home.join("mocktail"),
    })
}

/// What does not work with the installed Mocktail, from build.sh's mark in its folder
/// (`marker`) and the build this helper expects (`ALYAE_MOCKTAIL_BUILD`, from the module's
/// patches); None when it is that build.
fn build_warning(marker: Option<&str>, expected: &str) -> Option<&'static str> {
    match marker.map(str::trim) {
        Some(m) if m == expected => None,
        Some(_) => Some(
            "Mocktail was built with older Alyae patches: shift-lock cursor, Anti-AFK, the main's low-memory profile and the cursor fix may do nothing",
        ),
        None => Some(
            "This Mocktail is not Alyae's build: shift-lock cursor, Anti-AFK, the main's low-memory profile and the cursor fix do nothing",
        ),
    }
}

fn mkdir(p: &Path) {
    let _ = fs::create_dir_all(p);
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
}

/// Writes `text` to `path` (mode 0600) through a temporary file, so a reader never sees half.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("alyae-tmp");
    let _ = fs::remove_file(&tmp);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))?;
    fs::rename(&tmp, path)
}

fn link_dir(src: &Path, dest: &Path) {
    if src.exists() && fs::symlink_metadata(dest).is_err() {
        if let Some(d) = dest.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = symlink(src, dest);
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => {
            (x.dev(), x.ino()) == (y.dev(), y.ino())
                || (x.len() == y.len() && x.mtime() == y.mtime())
        }
        _ => false,
    }
}

/// The file name of the account's own copy of the Mocktail program: `mocktail_XII`.
fn program_name(req: &Request) -> String {
    format!("mocktail_{}", req.folder())
}

/// The name the copy had before accounts had numbers (from the account id); a game started
/// then still runs under it.
fn old_program_name(req: &Request) -> String {
    format!(
        "mocktail_alyae_{}",
        req.id.chars().take(12).collect::<String>()
    )
}

/// Whether the account's game runs, under either name of its program.
fn game_running(req: &Request) -> bool {
    [program_name(req), old_program_name(req)]
        .iter()
        .any(|n| already_running(Path::new(n)))
}

/// The account's own copy of the Mocktail program (a hard link where it can be one): Mocktail
/// derives its single-instance lock from its own path, so each account needs a path of its own.
fn account_program(m: &Mocktail, req: &Request, data_dir: &Path) -> PathBuf {
    let writable = std::ffi::CString::new(m.lib.as_os_str().as_bytes())
        // SAFETY: access(2) with a NUL-terminated path
        .is_ok_and(|p| unsafe { libc::access(p.as_ptr(), libc::W_OK) } == 0);
    let dir = if writable {
        m.lib.clone()
    } else {
        data_dir.join("bin")
    };
    mkdir(&dir);
    if dir != m.lib {
        // the libraries Mocktail loads from beside itself
        if let Ok(entries) = fs::read_dir(&m.lib) {
            for e in entries.flatten() {
                let n = e.file_name();
                let s = n.to_string_lossy();
                if (s.contains(".so") || s.starts_with("mocktail_")) && !alyae_game(&s) {
                    let dest = dir.join(&n);
                    if fs::symlink_metadata(&dest).is_err() {
                        let _ = symlink(e.path(), dest);
                    }
                }
            }
        }
    }
    let target = dir.join(program_name(req));
    if !same_file(&m.bin, &target) {
        let staging = target.with_extension("provision");
        let _ = fs::remove_file(&staging);
        if fs::hard_link(&m.bin, &staging).is_err()
            && let Err(e) = fs::copy(&m.bin, &staging)
        {
            fail(&format!("cannot copy Mocktail for this account: {e}"));
        }
        let _ = fs::set_permissions(&staging, fs::Permissions::from_mode(0o755));
        // renamed over the old one: a running instance keeps its file
        if let Err(e) = fs::rename(&staging, &target) {
            fail(&format!("cannot place Mocktail for this account: {e}"));
        }
    }
    target
}

/// Whether this account's Mocktail already runs (its program path names the account).
fn already_running(program: &Path) -> bool {
    running_pid(program).is_some()
}

/// The process ID of the running `program` (by its file name), if one runs.
fn running_pid(program: &Path) -> Option<i32> {
    let want = program
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    fs::read_dir("/proc").ok()?.flatten().find_map(|p| {
        let pid = p.file_name().to_str()?.parse::<i32>().ok()?;
        let c = fs::read(p.path().join("cmdline")).ok()?;
        let argv0 = c.split(|b| *b == 0).next().unwrap_or(&[]);
        (Path::new(std::ffi::OsStr::from_bytes(argv0)).file_name() == Some(want.as_os_str()))
            .then_some(pid)
    })
}

/// Alyae's copy of Mocktail for an account: `mocktail_<numeral>` (`program_name`), or the
/// `mocktail_alyae_<id>` it was before (`old_program_name`).
fn alyae_game(name: &str) -> bool {
    name.starts_with("mocktail_alyae_")
        || name
            .strip_prefix("mocktail_")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| "IVXLCDM".contains(c)))
}

/// Any Mocktail game: Alyae's copies and the plain program; not Mocktail's helper programs
/// (mocktail_webview_helper, mocktail_cookie_helper, ...).
fn any_game(name: &str) -> bool {
    alyae_game(name) || name == "mocktail"
}

/// The running games whose program name `pick` takes, as process IDs.
fn running_games(pick: fn(&str) -> bool) -> Vec<i32> {
    let Ok(procs) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    procs
        .flatten()
        .filter_map(|p| {
            let pid = p.file_name().to_str()?.parse::<i32>().ok()?;
            let c = fs::read(p.path().join("cmdline")).ok()?;
            let argv0 = c.split(|b| *b == 0).next().unwrap_or(&[]);
            let name = Path::new(std::ffi::OsStr::from_bytes(argv0)).file_name()?;
            pick(name.to_str()?).then_some(pid)
        })
        .collect()
}

/// Anti-AFK (Alyae runs it every few minutes): every running game moves the pointer a pixel and back once (as
/// anti-AFK does), so Roblox counts the account as active. Each game names its nudge file in
/// its environment; a game started before that existed has none and is skipped.
pub fn anti_afk() {
    let (mut nudged, mut old, mut failed) = (0, 0, Vec::new());
    for pid in running_games(alyae_game) {
        let env = fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
        let file = env
            .split(|b| *b == 0)
            .find_map(|kv| kv.strip_prefix(b"MOCKTAIL_NUDGE_FILE="))
            .map(|v| PathBuf::from(std::ffi::OsStr::from_bytes(v)));
        match file {
            None => old += 1,
            Some(f) => match fs::write(&f, format!("{:?}", std::time::SystemTime::now())) {
                Ok(()) => nudged += 1,
                Err(e) => failed.push(format!("{}: {e}", f.display())),
            },
        }
    }
    match nudge_report(nudged, old, &failed) {
        Ok(text) => status(&text),
        Err(text) => fail(&text),
    }
}

/// What Anti-AFK says after a round; Err when a nudge file could not be written.
fn nudge_report(nudged: usize, old: usize, failed: &[String]) -> Result<String, String> {
    let mut text = format!("{nudged} games nudged");
    if old > 0 {
        text += &format!(", {old} started before Anti-AFK existed (restart them)");
    }
    if failed.is_empty() {
        Ok(text)
    } else {
        Err(format!("{text}, {} not: {}", failed.len(), failed.join("; ")))
    }
}

/// The "Close all games" button: every Mocktail game, also ones Alyae did not start (plain
/// Mocktail), is asked to end, and made to after 4 s.
pub fn close_all() {
    let games = || running_games(any_game);
    let all = games();
    for pid in &all {
        // SAFETY: plain signal to a process ID
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < until && !games().is_empty() {
        std::thread::sleep(Duration::from_millis(200));
    }
    for pid in games() {
        // SAFETY: plain signal to a process ID
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    status(&format!("{} games closed", all.len()));
}

/// The Resume action, when Alyae opens: if this account's game still runs from before (Alyae
/// was closed, the game was left running), it counts as running again until it ends, and Stop
/// ends it; at its end a renewed cookie is saved as after Join Server. No game: a renewed cookie
/// a game left while Alyae was closed is saved; otherwise it ends at once, saying nothing.
pub fn resume(req: &Request) {
    let profile = req.profile();
    let cookie = req.cookie();
    let pid = [program_name(req), old_program_name(req)]
        .iter()
        .find_map(|n| running_pid(Path::new(n)));
    let Some(pid) = pid else {
        if profile.join(PICKUP).exists() {
            after_game(&profile, &cookie, false);
        }
        return;
    };
    let mut stop = Stop::listen();
    status("running (found again)");
    let alive = || Path::new(&format!("/proc/{pid}")).exists();
    while alive() {
        if stop.asked() {
            status("stopping");
            // SAFETY: plain signal to a process ID
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(4);
            while Instant::now() < until && alive() {
                std::thread::sleep(Duration::from_millis(100));
            }
            if alive() {
                // SAFETY: plain signal to a process ID
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    after_game(&profile, &cookie, stop.alyae_gone);
}

/// Left in a profile by a game that ended while Alyae was closed: its data/auth sign-in may hold
/// a renewed cookie for the next start of Alyae to save.
const PICKUP: &str = "renewed-cookie-pending";

/// The cookie Mocktail kept in data/auth when it is one Roblox renewed (not `cookie`).
fn renewed_cookie(profile: &Path, cookie: &str) -> Option<String> {
    let text = fs::read_to_string(profile.join("data/auth/roblox.cookie")).ok()?;
    let renewed = api::clean_cookie(&text);
    (renewed != cookie && renewed.starts_with("_|WARNING")).then_some(renewed)
}

/// After a game ended: a cookie Roblox renewed meanwhile (Mocktail keeps the session in
/// data/auth) goes to Alyae, and the plain-text sign-in files go. With Alyae closed nobody would
/// read it: Mocktail's file stays, marked for the next start of Alyae (`resume`).
fn after_game(profile: &Path, cookie: &str, alyae_gone: bool) {
    let _ = fs::remove_file(profile.join("roblox.cookie"));
    let (kept, pickup) = (profile.join("data/auth/roblox.cookie"), profile.join(PICKUP));
    if alyae_gone {
        if kept.exists() {
            let _ = write_private(&pickup, "");
        }
        return;
    }
    if let Some(renewed) = renewed_cookie(profile, cookie) {
        say(json!({ "set": { "cookie": renewed } }));
    }
    for f in [kept, pickup] {
        let _ = fs::remove_file(f);
    }
}
use std::os::unix::ffi::OsStrExt;

/// Logical CPUs for alts: the upper ~35% of physical cores (with their SMT siblings), so a busy
/// alt never shares a core with the main. A main is not pinned: with no alt running it has every
/// core, and the alts are fenced in anyway. None when there is nothing to split.
fn alt_cpus() -> Option<Vec<usize>> {
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    if n < 4 {
        return None;
    }
    let mut cores: Vec<((u32, u32), Vec<usize>)> = Vec::new();
    for cpu in 0..n {
        let t = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
        let read = |f: &str| {
            fs::read_to_string(format!("{t}/{f}"))
                .ok()?
                .trim()
                .parse::<u32>()
                .ok()
        };
        let key = (read("physical_package_id")?, read("core_id")?);
        match cores.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(cpu),
            None => cores.push((key, vec![cpu])),
        }
    }
    if cores.len() < 3 {
        return None;
    }
    let alt_cores = ((cores.len() as f64 * 0.35) as usize).max(1);
    Some(
        cores[cores.len() - alt_cores..]
            .iter()
            .flat_map(|(_, v)| v.clone())
            .collect(),
    )
}

/// The CPUs the game is held to: an alt's (`alt_cpus`); any other runs where the system puts it.
fn affinity(alt: bool) -> Option<Vec<usize>> {
    if alt { alt_cpus() } else { None }
}

fn set_affinity(cpus: &[usize]) {
    // SAFETY: a zeroed cpu_set_t is valid; CPU_SET indexes stay below CPU_SETSIZE (checked)
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus.iter().filter(|&&c| c < libc::CPU_SETSIZE as usize) {
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

/// The link Mocktail joins with: a place, and a server in it (job ID, or a private server's link
/// code). A follow carries the user instead of their server, so Roblox joins them its own way
/// (join type 1, as the friend Join button): a sub place's or a game-made server refuses a join by
/// job ID ("restricted"). Mocktail still wants a place: the one `follow_plan` found goes along.
fn uri(j: &Join) -> Option<String> {
    let mut u = format!("roblox://experiences/start?placeId={}", j.place?);
    if let Some(user) = j.follow {
        u += &format!("&userId={user}");
    } else if let Some(job) = &j.job {
        u += &format!("&gameInstanceId={job}");
    } else if let Some(code) = &j.link_code {
        u += &format!("&linkCode={code}");
    }
    Some(u)
}

/// Roblox's own shift-lock icon, which Alyae's Mocktail build compares uploads with: the
/// client's MouseLockedCursor.png from the newest payload (by folder time) that has one (not
/// every version ships it), else a `.roblox_lock_reference.png` put in `cursors` by hand.
fn lock_reference(payloads: &Path, cursors: &Path) -> Option<PathBuf> {
    let newest = fs::read_dir(payloads)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|p| {
            let icon = p.path().join("assets/content/textures/MouseLockedCursor.png");
            let time = p.metadata().and_then(|m| m.modified()).ok()?;
            icon.is_file().then_some((time, icon))
        })
        .max_by_key(|(time, _)| *time)
        .map(|(_, icon)| icon);
    newest.or_else(|| Some(cursors.join(".roblox_lock_reference.png")).filter(|f| f.is_file()))
}

/// Alyae's Mocktail build (shift-lock icon patch) draws the account's chosen picture from
/// `<data>/cursors` in place of Roblox's shift-lock icon, which it finds by comparing uploads
/// with Roblox's own (`lock_reference`). Without either, Roblox's icon stays.
fn shiftlock_cursor(req: &Request, m: &Mocktail) -> Vec<(String, String)> {
    let name = req.get("cursor");
    let dir = req.data_dir.join("cursors");
    let pic = dir.join(name);
    if name.is_empty() || name.contains('/') || !pic.is_file() {
        return Vec::new();
    }
    let Some(reference) = lock_reference(&m.data.join("payloads"), &dir) else {
        status(
            "shift-lock cursor: no installed Roblox version has MouseLockedCursor.png; Roblox's icon stays",
        );
        return Vec::new();
    };
    vec![
        ("MOCKTAIL_SHIFTLOCK_CURSOR".into(), pic.display().to_string()),
        ("MOCKTAIL_SHIFTLOCK_ORIGINAL".into(), reference.display().to_string()),
    ]
}

/// FastFlags: the low-cost set for alts; for a main, higher texture quality and the Windows
/// client's loading values; with no role, only the two every account gets.
fn fast_flags(role: Role) -> serde_json::Value {
    let mut f = match role {
        Role::Main => json!({
            "FStringRenderTextureBudgetByRam": "",
            "DFFlagTextureQualityOverrideEnabled": true,
            "DFIntTextureQualityOverride": 3,
            "DFIntDebugFRMQualityLevelOverride": 14,
            "FFlagDebugDisableDRS": true,
            "FFlagAutomaticDRS": false,
            "FFlagAutomaticDRSDisableHighDPIFramebufferHalving": true,
            "FFlagDebugGraphicsPreferOpenGL": true,
            // the Windows client's loading values where the Android ones are smaller: clothing
            // jobs, orphan texture budget, content hot cache and bundles, sound budget estimate,
            // a disk cache (rbx-storage) above the 2.5 GB it fills, and animation tracks per
            // character (Roblox drops the 51st at 50, and with it a late or missing animation)
            "FIntTextureCompositorActiveJobs": 8,
            "FIntRenderTextureOrphanMinBudgetMB": 64,
            "DFIntWrapContentProviderHotCacheSize": 20000000,
            "FIntWrapContentProviderMaxBundleCount": 64,
            "DFFlagSoundCacheUseBudgetUtilizationEstimator": true,
            "DFIntRbxStorageCapacityHighMB": 8192,
            "DFIntRbxStorageCapacityLowMB": 8192,
            "DFIntReplicatorAnimationTrackLimitPerAnimator": 256,
        }),
        Role::Alt => json!({
            "FStringRenderTextureBudgetByRam": "",
            "DFFlagTextureQualityOverrideEnabled": true,
            "DFIntTextureQualityOverride": 1,
            "DFIntDebugFRMQualityLevelOverride": 1,
            "FFlagDebugDisableDRS": true,
            "FFlagAutomaticDRS": false,
            "FFlagAutomaticDRSDisableHighDPIFramebufferHalving": true,
        }),
        Role::AsIs => json!({}),
    };
    // Roblox keeps decoded animation clips on Windows but not in the Android build Mocktail runs
    f["FFlagAnimationClipMemCacheEnabled"] = json!(true);
    f["FStringRobloxAnalyticsURL"] = json!("");
    f
}

/// Before each launch: Roblox's frame cap follows the account's FPS cap, if it has one; alts
/// are kept at graphics level 1 and muted. Any other graphics level, sensitivity and volume are
/// left to Roblox's menu and the Settings button.
fn engine_settings(profile: &Path, role: Role, fps_cap: Option<u32>) {
    let mut props: Vec<_> = fps_cap
        .map(|f| settings_xml::prop("int", "FramerateCap", f))
        .into_iter()
        .collect();
    if role == Role::Alt {
        props.extend(settings_xml::quality(1));
        props.push(settings_xml::prop("float", "MasterVolume", 0));
    }
    if props.is_empty() {
        return;
    }
    if let Err(e) = settings_xml::apply(&settings_file(profile), &props) {
        fail(&format!("cannot write Roblox's settings: {e}"));
    }
}

fn settings_file(profile: &Path) -> PathBuf {
    profile.join("data/files/appData/GlobalBasicSettings_13.xml")
}

/// The Settings button: sets the graphics level, camera sensitivity, volume and the rest of its
/// form in Roblox's own settings file. A value left at "keep" (or empty) stays as it is.
pub fn graphics(req: &Request) {
    if game_running(req) {
        fail("close this account's Roblox first: it rewrites its settings when it closes");
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
    let profile = req.profile();
    match settings_xml::apply(&settings_file(&profile), &props) {
        Ok(()) => status(&format!("{} settings set", props.len())),
        Err(e) => fail(&format!("cannot write Roblox's settings: {e}")),
    }
}

/// What Reset files removes from a profile: Roblox's app storage, its caches and downloaded
/// assets, Mocktail's saved sign-in and logs; all are made again on the next launch, the sign-in
/// from Alyae's vault. Kept: Roblox's settings file, the device ID, the window and Mocktail
/// settings, the links to the shared client. Returns the bytes freed.
fn reset_profile(profile: &Path) -> u64 {
    let data = profile.join("data");
    clear(&data.join("files/appData"), &["GlobalBasicSettings_13.xml"])
        + clear(&data.join("files"), &["appData"])
        + clear(&data.join("mocktail"), &["payloads", "current.json"])
        + clear(
            &data,
            &["payloads", "rbx_bin", "current.json", "mocktail", "files"],
        )
        + clear(&profile.join("cache"), &["mesa_shader_cache"])
        + remove(&profile.join("state/logs"))
        + remove(&profile.join("roblox.cookie"))
}

/// The Reset files button.
pub fn reset(req: &Request) {
    if game_running(req) {
        fail("close this account's Roblox first");
    }
    let profile = req.profile();
    if !profile.is_dir() {
        fail("this account has no files yet");
    }
    let freed = reset_profile(&profile);
    status(&format!("files reset, {} MB freed", freed / 1_000_000));
}

/// The resolved profile folder when it is one of Alyae's own (inside `data_dir/profiles`, links
/// resolved, so a link pointing out of it does not count); None for any other folder.
fn owned_profile(profile: &Path, data_dir: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(data_dir.join("profiles")).ok()?;
    let p = fs::canonicalize(profile).ok()?;
    (p != root && p.starts_with(&root)).then_some(p)
}

/// Run before the account is removed from Alyae: its copy of the Mocktail program goes (under
/// either name), and its profile folder when it is inside Alyae's profiles folder.
pub fn forget(req: &Request) {
    if game_running(req) {
        fail("close this account's Roblox first");
    }
    let mut problems = Vec::new();
    let mut gone = |f: &Path| match fs::remove_file(f) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            problems.push(format!("{}: {e}", f.display()))
        }
        _ => {}
    };
    let mut dirs = vec![req.data_dir.join("bin")];
    dirs.extend(find_mocktail().map(|m| m.lib));
    for d in dirs {
        for name in [program_name(req), old_program_name(req)] {
            let program = d.join(name);
            gone(&program.with_extension("provision"));
            gone(&program);
        }
    }
    let profile = req.profile();
    if let Some(p) = owned_profile(&profile, &req.data_dir)
        && let Err(e) = fs::remove_dir_all(&p)
    {
        problems.push(format!("{}: {e}", p.display()));
    }
    if !problems.is_empty() {
        fail(&format!("cannot delete {}", problems.join("; ")));
    }
    status("files deleted");
}

/// RAM that must be free (MemAvailable) before another client starts: many clients loading a
/// big map at once froze a 32 GB PC.
const FREE_TO_START_MB: u64 = 4096;

/// MemAvailable from `/proc/meminfo` text, in MB.
fn mem_available_mb(meminfo: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

/// Waits until there is room for one more client, at most `crate::MEMORY_WAIT`; a stop ends the
/// wait and the helper.
fn wait_for_memory(stop: &mut Stop) {
    let free = || {
        fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| mem_available_mb(&t))
            .unwrap_or(u64::MAX)
    };
    crate::wait_for_memory(free, || FREE_TO_START_MB, || stop.end_if_asked());
}

/// What a launch does to Mocktail's window.
#[derive(Debug, PartialEq)]
enum Window {
    /// as Mocktail left it
    AsIs,
    /// not fullscreen, at the size Mocktail kept (its own 1280x720 the first time). Mocktail
    /// sizes in logical desktop units: the display's pixel size (from /sys/class/drm) is wrong
    /// under scaling, and which display the window opens on is not known
    Windowed,
    /// fullscreen or not, at this size
    Sized { fullscreen: bool, w: u32, h: u32 },
}

/// The account's Window as Mocktail's. Empty: a main fullscreen, an alt small, no role as
/// Mocktail left it.
fn window_plan(mode: &str, role: Role) -> Window {
    let (w, h) = settings_xml::SMALL;
    match (mode, role) {
        ("small", _) | ("", Role::Alt) => Window::Sized {
            fullscreen: false,
            w,
            h,
        },
        ("windowed", _) => Window::Windowed,
        ("fullscreen", _) | ("", Role::Main) => Window::Sized {
            fullscreen: true,
            w: 1920,
            h: 1080,
        },
        _ => Window::AsIs,
    }
}

/// Mocktail's saved window state with fullscreen off and the rest (size, place, maximized)
/// kept; None when there is no state object to keep.
fn windowed_state(saved: &str) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(saved).ok()?;
    v.as_object_mut()?.insert("fullscreen".into(), json!(false));
    Some(v.to_string())
}

/// Mocktail's config.yaml: what every account runs with, and what its role, FPS cap and Window
/// ask for. A key left out is Mocktail's own.
fn mocktail_config(role: Role, fps_limit: Option<u32>, window: &Window) -> String {
    let mut c = String::from("version: 1\ndevice: pc-windows-11\ngraphics:\n  backend: opengl\n");
    if let Some(f) = fps_limit {
        c += &format!("  frame_rate_limit: {f}\n  vsync: off\n");
    }
    c += "performance:\n  multithreaded_rendering: true\n";
    match role {
        Role::Main => c += "  physics_worker_mode: throughput\n  gamemode: auto\n",
        Role::Alt => c += "  physics_worker_mode: auto\n  gamemode: off\n",
        Role::AsIs => {}
    }
    c += "window:\n";
    if let Window::Sized { w, h, .. } = window {
        c += &format!("  width: {w}\n  height: {h}\n");
    }
    c + "  high_dpi: false\nupdates:\n  automatic: false\n"
}

/// The environment a launch sets from the account's role, FPS cap and Window: no role with
/// those fields empty sets none of it but the two fast flags every account gets.
fn role_env(role: Role, fps_limit: Option<u32>, window: &Window) -> Vec<(String, String)> {
    let mut env: Vec<(&str, String)> = Vec::new();
    if let Window::Sized { w, h, .. } = window {
        env.push(("MOCKTAIL_WIN_WIDTH", w.to_string()));
        env.push(("MOCKTAIL_WIN_HEIGHT", h.to_string()));
    }
    if let Some(f) = fps_limit {
        env.extend([
            ("vblank_mode", "0".into()),
            ("MOCKTAIL_FRAME_RATE_LIMIT", f.to_string()),
            ("MOCKTAIL_VSYNC", "off".into()),
            ("SDL_VIDEO_WAYLAND_ALLOW_TEARING", "1".into()),
        ]);
    }
    match role {
        Role::Main => env.extend([
            ("MOCKTAIL_GRAPHICS_QUALITY", "manual".into()),
            ("MOCKTAIL_PHYSICS_WORKER_MODE", "throughput".into()),
            ("MOCKTAIL_F11_FULLSCREEN", "1".into()),
            ("MOCKTAIL_FULLSCREEN_READINESS", "1".into()),
            // Alyae's Mocktail build (patch 10): a main loses Mocktail's phone-sized memory
            // caps, so textures, meshes, sounds and animations stay loaded instead of being
            // evicted and fetched again; alts keep them
            ("MOCKTAIL_LOW_MEMORY_PROFILE", "0".into()),
        ]),
        Role::Alt => env.extend([
            ("MOCKTAIL_GRAPHICS_QUALITY", "1".into()),
            ("MOCKTAIL_PHYSICS_WORKER_MODE", "auto".into()),
            ("MOCKTAIL_F11_FULLSCREEN", "0".into()),
            ("MOCKTAIL_FULLSCREEN_READINESS", "0".into()),
            ("SDL_AUDIODRIVER", "dummy".into()),
            ("PULSE_SERVER", "/dev/null".into()),
            ("ALSA_CARD", "Dummy".into()),
            ("MOCKTAIL_ENGINE_PUMP_REST", "sleep".into()),
        ]),
        Role::AsIs => {}
    }
    let flags = fast_flags(role).to_string();
    env.push(("MOCKTAIL_CLIENT_SETTINGS_OVERRIDES_JSON", flags.clone()));
    env.push(("MOCKTAIL_FAST_FLAGS_JSON", flags));
    env.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn prepare_profile(
    m: &Mocktail,
    data_dir: &Path,
    profile: &Path,
    role: Role,
    fps_limit: Option<u32>,
    window: &Window,
) {
    for sub in ["data/mocktail", "cache/mocktail"] {
        mkdir(&profile.join(sub));
    }
    // the Roblox client (payloads) is shared; the chosen version is copied
    link_dir(&m.data.join("payloads"), &profile.join("data/payloads"));
    if let Ok(cur) = fs::read(m.data.join("current.json")) {
        let _ = fs::write(profile.join("data/current.json"), &cur);
    }
    // downloaded assets: every profile keeps its own (Mocktail makes the folder), as Roblox
    // deletes files its own index evicts, also ones another account's index still lists. A
    // link to the folder alts once shared goes; only the link: the shared files stay until
    // the last alt using it has let go, then the empty folder goes
    for dest in [
        profile.join("data/mocktail/rbx-storage"),
        profile.join("data/rbx-storage"),
    ] {
        if fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) {
            let _ = fs::remove_file(&dest);
        }
    }
    let _ = fs::remove_dir(data_dir.join("shared_rbx_storage"));
    let shaders = data_dir.join("shared_mesa_shader_cache");
    mkdir(&shaders);
    link_dir(&shaders, &profile.join("cache/mesa_shader_cache"));

    let put = |file: &Path, text: &str| {
        if let Err(e) = write_private(file, text) {
            fail(&format!("cannot write {}: {e}", file.display()));
        }
    };
    put(
        &profile.join("config/config.yaml"),
        &mocktail_config(role, fps_limit, window),
    );
    let state = profile.join("state/window-state.json");
    match window {
        Window::AsIs => {}
        Window::Windowed => match fs::read_to_string(&state)
            .ok()
            .and_then(|t| windowed_state(&t))
        {
            Some(t) => put(&state, &t),
            // nothing to keep: Mocktail opens windowed at its own size
            None => {
                let _ = fs::remove_file(&state);
            }
        },
        Window::Sized { fullscreen, w, h } => put(
            &state,
            &json!({
                "fullscreen": fullscreen, "maximized": false, "schema_version": 1,
                "windowed": { "has_position": false, "height": h, "width": w },
            })
            .to_string(),
        ),
    }
    // written ahead so concurrent first starts do not race on it inside Mocktail
    let platform = r#"{"profile_revision":"device-v1-pc-windows-11-t0-m1-k1","schema_version":1}"#;
    for d in [
        profile.join("data/mocktail/android/data"),
        profile.join("data/android/data"),
        profile.join("data"),
    ] {
        let f = d.join(".mocktail-platform-profile.json");
        if !f.exists() {
            let _ = write_private(&f, platform);
        }
    }
}

/// The sign-in file Mocktail reads (MOCKTAIL_COOKIE_FILE).
fn write_cookie(profile: &Path, cookie: &str, tracker: Option<&str>) -> PathBuf {
    let mut line = format!(".ROBLOSECURITY={cookie}\n");
    if let Some(t) = tracker {
        line += &format!("RBXEventTrackerV2={t}\n");
    }
    let path = profile.join("roblox.cookie");
    if let Err(e) = write_private(&path, &line) {
        fail(&format!("cannot write the sign-in file: {e}"));
    }
    path
}

fn runtime_dir(req: &Request, profile: &Path) -> PathBuf {
    let host = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let dir = match &host {
        Some(h) if h.is_dir() => h.join("alyae").join(req.folder()),
        _ => profile.join("run"),
    };
    mkdir(&dir);
    if let Some(h) = host {
        let wl = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
        for name in [
            "bus",
            "systemd",
            "pulse",
            "pipewire-0",
            "pipewire-0-manager",
            "wayland-0",
            wl.as_str(),
        ] {
            let (src, dest) = (h.join(name), dir.join(name));
            if fs::symlink_metadata(&dest).is_ok() && !dest.exists() {
                let _ = fs::remove_file(&dest);
            }
            if src.exists() && fs::symlink_metadata(&dest).is_err() {
                let _ = symlink(src, dest);
            }
        }
    }
    dir
}

fn tail(path: &Path, from: u64) -> String {
    let text = fs::read(path)
        .map(|b| String::from_utf8_lossy(&b[(from as usize).min(b.len())..]).into_owned())
        .unwrap_or_default();
    text.lines()
        .rev()
        .find(|l| {
            let l = l.to_lowercase();
            l.contains("error") || l.contains("failed") || l.contains("fatal")
        })
        .map(|l| l.trim().chars().take(200).collect())
        .unwrap_or_default()
}

/// Held while one account's client starts (as on Windows): starting many accounts at once runs
/// their Mocktail setups one after another instead of all together, so the desktop and the
/// accounts already playing keep their CPU and disk. Released when the client has run a few
/// seconds or ended; a holder that dies releases it with its file.
struct LaunchTurn(#[allow(dead_code)] fs::File);

impl LaunchTurn {
    const HOLD: Duration = Duration::from_secs(5);

    /// Waits for the turn; a stop meanwhile ends the helper.
    fn take(data_dir: &Path, stop: &mut Stop) -> Option<Self> {
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(data_dir.join("launch.lock"))
            .ok()?;
        use std::os::fd::AsRawFd;
        let mut said = false;
        // SAFETY: flock(2) on a file this function owns
        while unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
                return None;
            }
            if !said {
                status("waiting for another account to start");
                said = true;
            }
            stop.end_if_asked();
            std::thread::sleep(Duration::from_millis(250));
        }
        Some(LaunchTurn(f))
    }
}

pub fn play(req: &Request) {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: paste the .ROBLOSECURITY value into Cookie");
    }
    // a stop from Alyae ends the waits below, or the game once it runs; Alyae closing leaves
    // the game running
    let mut stop = Stop::listen();
    let Some(m) = find_mocktail() else {
        fail(
            "Mocktail is not installed (looked in ~/.local/lib/mocktail, /usr/lib/mocktail and PATH)",
        );
    };
    let data_dir = &req.data_dir;
    let role = req.role();
    let fps_limit = req
        .fps()
        .map(|f| if f == 0 || f >= 1000 { 10000 } else { f });
    let program = account_program(&m, req, data_dir);
    if already_running(&program) {
        fail("this account is already running");
    }
    let join = join_from(req);
    let profile = req.profile();
    mkdir(&profile);
    // Mocktail's saved sign-in from a run whose helper was killed: the files written below do not
    // replace it, and at the end it would pass for a renewed cookie. (One left for pickup was
    // saved when Alyae opened, by `resume`.)
    let _ = fs::remove_file(profile.join("data/auth/roblox.cookie"));
    let _ = fs::remove_file(profile.join(PICKUP));
    // a second copy earlier versions wrote, which Mocktail never read
    let _ = fs::remove_file(profile.join("data/mocktail/roblox.cookie"));
    // one account started on its own does not wait for free RAM. The turn comes after the
    // memory wait, so an account waiting for memory does not hold up one started on its own
    if req.input("_alone") != "1" {
        wait_for_memory(&mut stop);
    }
    let mut turn = LaunchTurn::take(data_dir, &mut stop);
    stop.end_if_asked();
    // a folder named by the account id (before accounts had numbers) gets its numeral, in the
    // turn and only while no game of the account's uses it; the old copy of Mocktail goes
    let profiles = data_dir.join("profiles");
    let old_running = already_running(Path::new(&old_program_name(req)));
    if let Err(e) = rename_old(
        &profiles.join(&req.id),
        &profiles.join(req.folder()),
        old_running,
    ) {
        fail(&e);
    }
    let profile = req.profile();
    if !old_running {
        let _ = fs::remove_file(program.with_file_name(old_program_name(req)));
    }
    status("starting");
    let window = window_plan(req.window(), role);
    prepare_profile(
        &m,
        data_dir,
        &profile,
        role,
        fps_limit.map(|f| if f == 10000 { 1000 } else { f }),
        &window,
    );
    engine_settings(&profile, role, fps_limit);
    let tracker_file = profile.join("browser_tracker");
    let tracker = fs::read_to_string(&tracker_file)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| api::browser_id(t).is_some())
        .or_else(|| {
            let t = api::new_browser_tracker()?;
            let _ = write_private(&tracker_file, &format!("{t}\n"));
            Some(t)
        });
    let run = runtime_dir(req, &profile);

    let mut args: Vec<String> = vec!["--windowed".into(), "--graphics".into(), "opengl".into()];
    if let Some(u) = uri(&join) {
        args.push("--launch-uri".into());
        args.push(u);
    }
    let p = |s: &str| profile.join(s).to_string_lossy().into_owned();
    let mut env: Vec<(String, String)> = vec![
        ("XDG_RUNTIME_DIR".into(), run.to_string_lossy().into_owned()),
        ("MOCKTAIL_RUNTIME_ROOT".into(), p("")),
        ("MOCKTAIL_DATA_ROOT".into(), p("data")),
        ("MOCKTAIL_CONFIG_ROOT".into(), p("config")),
        ("MOCKTAIL_CACHE_ROOT".into(), p("cache")),
        ("MOCKTAIL_STATE_ROOT".into(), p("state")),
        ("XDG_DATA_HOME".into(), p("data")),
        ("XDG_CONFIG_HOME".into(), p("config")),
        ("XDG_CACHE_HOME".into(), p("cache")),
        ("XDG_STATE_HOME".into(), p("state")),
        ("MOCKTAIL_COOKIE_FILE".into(), p("roblox.cookie")),
        // Alyae's Mocktail build (patch 11): writing this file asks the game for one nudge now
        ("MOCKTAIL_NUDGE_FILE".into(), p("nudge")),
        ("MOCKTAIL_DISABLE_FAILURE_DIALOG".into(), "1".into()),
        ("MOCKTAIL_GRAPHICS_BACKEND".into(), "opengl".into()),
        // a Windows PC with mouse and keyboard: Roblox shows its PC layout and settings
        ("MOCKTAIL_DEVICE_PROFILE".into(), "pc-windows-11".into()),
        ("MOCKTAIL_TOUCH_MODE".into(), "0".into()),
        ("MOCKTAIL_MOUSE_MODE".into(), "1".into()),
        ("MOCKTAIL_KEYBOARD_MODE".into(), "1".into()),
        ("MOCKTAIL_WIN_HIGH_DPI".into(), "0".into()),
        ("MOCKTAIL_MULTITHREADED_RENDERING".into(), "1".into()),
    ];
    env.extend(role_env(role, fps_limit, &window));
    env.extend(shiftlock_cursor(req, &m));
    let meta = m.data.join("metadata");
    if meta.is_dir() {
        for (k, f) in [
            (
                "MOCKTAIL_COMPATIBILITY_MANIFEST",
                "roblox_compatibility.json",
            ),
            (
                "MOCKTAIL_BOOTSTRAP_SOURCES_PATH",
                "roblox_bootstrap_sources.json",
            ),
            (
                "MOCKTAIL_HOST_ABI_PROFILE_FILE",
                "roblox_host_abi_reference.json",
            ),
            (
                "MOCKTAIL_UPDATE_SIGNING_TRUST_PATH",
                "roblox_signing_certificates.json",
            ),
        ] {
            env.push((k.into(), meta.join(f).to_string_lossy().into_owned()));
        }
    }
    let mut ld = vec![m.lib.to_string_lossy().into_owned()];
    if let Some(d) = program.parent().filter(|d| *d != m.lib) {
        ld.push(d.to_string_lossy().into_owned());
    }
    if let Ok(old) = std::env::var("LD_LIBRARY_PATH") {
        ld.extend(old.split(':').filter(|s| !s.is_empty()).map(String::from));
    }
    env.push(("LD_LIBRARY_PATH".into(), ld.join(":")));

    let alt = role == Role::Alt;
    let affinity = affinity(alt);
    let mut cmd = if alt && on_path("systemd-run") {
        // alts in a low-weight scope: the desktop and the main come first
        let mut c = Command::new("systemd-run");
        c.args([
            "--user",
            "--scope",
            "--quiet",
            "-pMemoryHigh=900M",
            "-pCPUWeight=1",
            "-pIOWeight=1",
        ]);
        if let Some(alt) = &affinity {
            c.arg(format!(
                "-pAllowedCPUs={}",
                alt.iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        c.arg("--").arg(&program);
        c
    } else {
        Command::new(&program)
    };
    cmd.args(&args);
    for key in std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("MOCKTAIL_"))
    {
        cmd.env_remove(key);
    }
    cmd.envs(env);
    let log_path = profile.join("alyae.log");
    if fs::metadata(&log_path).is_ok_and(|m| m.len() > 8 << 20) {
        let _ = fs::rename(&log_path, profile.join("alyae.log.1"));
    }
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .unwrap_or_else(|e| fail(&format!("cannot open {}: {e}", log_path.display())));
    let log_start = log.metadata().map_or(0, |m| m.len());
    let _ = std::io::Write::write_all(
        &mut &log,
        format!("\n=== Alyae launch {} ===\n", std::process::id()).as_bytes(),
    );
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone().unwrap_or_else(|e| fail(&e.to_string())))
        .stderr(log);
    // SAFETY: only async-signal-safe calls between fork and exec
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            if let Some(cpus) = &affinity {
                set_affinity(cpus);
            }
            if alt {
                // an alt gives way to a main when the PC is busy (raising a main needs root;
                // lowering the alts does the same for it): nice 5, inherited by everything the
                // game starts. Not lower: an alt starved of CPU can lose its connection. No
                // realtime or raised priority for it afterwards either
                libc::nice(5);
                let zero = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_RTPRIO, &zero);
                libc::setrlimit(libc::RLIMIT_NICE, &zero);
            }
            Ok(())
        });
    }
    // written last, so a launch that fails before the game starts leaves no copy of the cookie
    let cookie_file = write_cookie(&profile, &cookie, tracker.as_deref());
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap_or_else(|e| {
        let _ = fs::remove_file(&cookie_file);
        fail(&format!("cannot start Mocktail: {e}"))
    });
    let pid = child.id() as i32;
    say(json!({ "status": "running" }));
    let code = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break s;
        }
        if turn.is_some() && started.elapsed() >= LaunchTurn::HOLD {
            turn = None;
        }
        if stop.asked() {
            status("stopping");
            // SAFETY: plain kill(2) on the game's own process group
            unsafe { libc::kill(-pid, libc::SIGTERM) };
            let until = Instant::now() + Duration::from_secs(4);
            while Instant::now() < until && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(100));
            }
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            break child.wait().unwrap_or_else(|e| fail(&e.to_string()));
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    after_game(&profile, &cookie, stop.alyae_gone);
    if !code.success() && started.elapsed() < Duration::from_secs(20) {
        let why = tail(&log_path, log_start);
        fail(&if why.is_empty() {
            format!(
                "Mocktail stopped right away ({code}); see {}",
                log_path.display()
            )
        } else {
            why
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_uris() {
        let j = Join {
            place: Some(1),
            job: Some("j".into()),
            link_code: None,
            follow: None,
        };
        assert_eq!(
            uri(&j).as_deref(),
            Some("roblox://experiences/start?placeId=1&gameInstanceId=j")
        );
        // a follow joins by user, not by the user's server; without a place there is no link
        // (Mocktail refuses one: "requires a positive placeId")
        let j = Join {
            place: Some(2),
            job: Some("s".into()),
            link_code: None,
            follow: Some(9),
        };
        assert_eq!(
            uri(&j).as_deref(),
            Some("roblox://experiences/start?placeId=2&userId=9")
        );
        let j = Join {
            place: None,
            job: None,
            link_code: None,
            follow: Some(9),
        };
        assert!(uri(&j).is_none());
        assert!(
            uri(&Join {
                place: None,
                job: None,
                link_code: None,
                follow: None
            })
            .is_none()
        );
    }

    /// Reset keeps the settings file, the device ID and the shared folders behind links.
    #[test]
    fn reset_keeps_what_it_should() {
        let root = std::env::temp_dir().join(format!("alyae-reset-{}", std::process::id()));
        let (p, shared) = (root.join("profile"), root.join("shared"));
        for d in [
            "data/files/appData/LocalStorage",
            "data/ContentProvider_1",
            "data/mocktail/x",
            "cache/clientsettings",
            "state/logs",
        ] {
            fs::create_dir_all(p.join(d)).unwrap();
        }
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("asset"), "shared").unwrap();
        symlink(&shared, p.join("data/rbx-storage")).unwrap();
        symlink(&shared, p.join("data/payloads")).unwrap();
        fs::write(
            p.join("data/files/appData/GlobalBasicSettings_13.xml"),
            "settings",
        )
        .unwrap();
        fs::write(
            p.join("data/files/appData/LocalStorage/appStorage.json"),
            "app",
        )
        .unwrap();
        fs::write(p.join("data/files/appData/rbx-storage.db"), "index").unwrap();
        fs::write(p.join("browser_tracker"), "id").unwrap();
        fs::write(p.join("data/mocktail/current.json"), "{}").unwrap();
        assert!(reset_profile(&p) > 0);
        assert!(
            p.join("data/files/appData/GlobalBasicSettings_13.xml")
                .exists()
        );
        assert!(
            p.join("browser_tracker").exists() && p.join("data/mocktail/current.json").exists()
        );
        assert!(fs::symlink_metadata(p.join("data/payloads")).is_ok());
        for gone in [
            "data/files/appData/LocalStorage",
            "data/files/appData/rbx-storage.db",
            "data/ContentProvider_1",
            "data/rbx-storage",
            "data/mocktail/x",
            "cache/clientsettings",
            "state/logs",
        ] {
            assert!(fs::symlink_metadata(p.join(gone)).is_err(), "{gone} left");
        }
        assert_eq!(
            fs::read_to_string(shared.join("asset")).unwrap(),
            "shared",
            "followed a link"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Only folders inside Alyae's own profiles folder are Alyae's to delete; folders elsewhere
    /// and links pointing out are not.
    #[test]
    fn owned_profiles_only() {
        let root = std::env::temp_dir().join(format!("alyae-forget-{}", std::process::id()));
        let (data, elsewhere) = (root.join("data"), root.join("elsewhere/profile"));
        let own = data.join("profiles/abc");
        fs::create_dir_all(&own).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, data.join("profiles/link")).unwrap();
        assert_eq!(
            owned_profile(&own, &data),
            Some(fs::canonicalize(&own).unwrap())
        );
        // the same folder named through a detour still counts
        assert!(owned_profile(&data.join("profiles/../profiles/abc"), &data).is_some());
        assert_eq!(owned_profile(&elsewhere, &data), None);
        assert_eq!(owned_profile(&data.join("profiles/link"), &data), None);
        assert_eq!(owned_profile(&data.join("profiles/../.."), &data), None);
        assert_eq!(owned_profile(&data.join("profiles"), &data), None);
        assert_eq!(owned_profile(&data, &data), None);
        assert_eq!(owned_profile(&data.join("profiles/missing"), &data), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn free_memory_from_meminfo() {
        let t = "MemTotal:       32768000 kB\nMemFree:         1000000 kB\nMemAvailable:    6291456 kB\n";
        assert_eq!(mem_available_mb(t), Some(6144));
        assert_eq!(mem_available_mb("MemTotal: 1 kB\n"), None);
    }

    /// The account's Window picks Mocktail's; empty keeps the role's (main fullscreen, alt
    /// small, no role as Mocktail left it). Windowed leaves the size to Mocktail.
    #[test]
    fn window_by_choice_or_role() {
        let (small, full) = (
            Window::Sized {
                fullscreen: false,
                w: 800,
                h: 600,
            },
            Window::Sized {
                fullscreen: true,
                w: 1920,
                h: 1080,
            },
        );
        assert_eq!(window_plan("", Role::Main), full);
        assert_eq!(window_plan("", Role::Alt), small);
        assert_eq!(window_plan("", Role::AsIs), Window::AsIs);
        assert_eq!(window_plan("small", Role::Main), small);
        assert_eq!(window_plan("windowed", Role::Alt), Window::Windowed);
        assert_eq!(window_plan("fullscreen", Role::AsIs), full);
    }

    /// Windowed turns fullscreen off and keeps the size and place Mocktail saved.
    #[test]
    fn windowed_keeps_the_saved_size() {
        let saved = r#"{"fullscreen":true,"maximized":false,"schema_version":1,
            "windowed":{"has_position":true,"x":10,"y":20,"width":1500,"height":900},
            "configured":{"width":1280,"height":720}}"#;
        let v: serde_json::Value = serde_json::from_str(&windowed_state(saved).unwrap()).unwrap();
        assert_eq!(v["fullscreen"], false);
        assert_eq!(v["windowed"]["width"], 1500);
        assert_eq!(v["configured"]["height"], 720);
        assert_eq!(windowed_state("not json"), None);
        assert_eq!(windowed_state("[1]"), None);
        // no size of Alyae's in the config either
        assert!(!mocktail_config(Role::Main, None, &Window::Windowed).contains("width"));
    }

    /// No role and no FPS cap or Window: nothing of the roles' is forced on the game.
    #[test]
    fn no_role_leaves_roblox_as_it_is() {
        let env = role_env(Role::AsIs, None, &Window::AsIs);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "MOCKTAIL_CLIENT_SETTINGS_OVERRIDES_JSON",
                "MOCKTAIL_FAST_FLAGS_JSON"
            ]
        );
        let flags = fast_flags(Role::AsIs);
        assert!(flags.get("DFIntTextureQualityOverride").is_none());
        assert!(flags.get("FFlagDebugDisableDRS").is_none());
        let config = mocktail_config(Role::AsIs, None, &Window::AsIs);
        for forced in ["frame_rate_limit", "vsync", "physics_worker_mode", "gamemode", "width"] {
            assert!(!config.contains(forced), "{forced} in {config}");
        }
        // what the account's own fields ask for still applies
        let env = role_env(Role::AsIs, Some(60), &Window::AsIs);
        assert!(env.contains(&("MOCKTAIL_FRAME_RATE_LIMIT".into(), "60".into())));
        // a main and an alt keep theirs
        let main = role_env(Role::Main, Some(330), &window_plan("", Role::Main));
        assert!(main.contains(&("MOCKTAIL_LOW_MEMORY_PROFILE".into(), "0".into())));
        assert!(main.contains(&("MOCKTAIL_WIN_WIDTH".into(), "1920".into())));
        let alt = role_env(Role::Alt, Some(1), &window_plan("", Role::Alt));
        assert!(alt.contains(&("MOCKTAIL_GRAPHICS_QUALITY".into(), "1".into())));
    }

    /// No role and no FPS cap: Roblox's settings file is left alone.
    #[test]
    fn no_role_leaves_roblox_settings() {
        let dir = std::env::temp_dir().join(format!("alyae-asis-{}", std::process::id()));
        engine_settings(&dir, Role::AsIs, None);
        assert!(!settings_file(&dir).exists());
        let _ = fs::remove_dir_all(dir);
    }

    /// A launch writes only the files Mocktail reads under the roots Alyae gives it.
    #[test]
    fn profile_has_only_what_mocktail_reads() {
        let root = std::env::temp_dir().join(format!("alyae-prep-{}", std::process::id()));
        let (data, p) = (root.join("alyae"), root.join("profile"));
        let m = Mocktail {
            bin: root.join("lib/mocktail"),
            lib: root.join("lib"),
            data: root.join("share"),
        };
        fs::create_dir_all(m.data.join("payloads")).unwrap();
        fs::write(m.data.join("current.json"), "{}").unwrap();
        prepare_profile(&m, &data, &p, Role::Main, Some(60), &window_plan("small", Role::Main));
        for made in [
            "config/config.yaml",
            "state/window-state.json",
            "data/current.json",
            "data/payloads",
        ] {
            assert!(fs::symlink_metadata(p.join(made)).is_ok(), "{made} missing");
        }
        for unread in [
            "config/fflags.json",
            "config/mocktail/config.yaml",
            "state/mocktail/window-state.json",
            "data/mocktail/current.json",
            "data/mocktail/payloads",
        ] {
            assert!(fs::symlink_metadata(p.join(unread)).is_err(), "{unread} written");
        }
        let _ = fs::remove_dir_all(root);
    }

    /// An alt's link to the once-shared asset cache goes, the files behind it stay, and the
    /// shared folder goes once empty.
    #[test]
    fn alts_keep_their_own_assets() {
        let root = std::env::temp_dir().join(format!("alyae-assets-{}", std::process::id()));
        let (data, p) = (root.join("alyae"), root.join("profile"));
        let m = Mocktail {
            bin: root.join("lib/mocktail"),
            lib: root.join("lib"),
            data: root.join("share"),
        };
        let shared = data.join("shared_rbx_storage");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("asset"), "a").unwrap();
        fs::create_dir_all(p.join("data")).unwrap();
        symlink(&shared, p.join("data/rbx-storage")).unwrap();
        prepare_profile(&m, &data, &p, Role::Alt, Some(1), &Window::AsIs);
        assert!(fs::symlink_metadata(p.join("data/rbx-storage")).is_err());
        assert_eq!(fs::read_to_string(shared.join("asset")).unwrap(), "a");
        fs::remove_file(shared.join("asset")).unwrap();
        prepare_profile(&m, &data, &p, Role::Alt, Some(1), &Window::AsIs);
        assert!(!shared.exists());
        let _ = fs::remove_dir_all(root);
    }

    /// After a game: with Alyae there the renewed cookie is read and every sign-in file goes;
    /// with Alyae closed Mocktail's stays, marked, for the next start of Alyae.
    #[test]
    fn renewed_cookie_waits_for_alyae() {
        let p = std::env::temp_dir().join(format!("alyae-renew-{}", std::process::id()));
        let kept = p.join("data/auth/roblox.cookie");
        fs::create_dir_all(kept.parent().unwrap()).unwrap();
        let write = || {
            fs::write(p.join("roblox.cookie"), ".ROBLOSECURITY=old\n").unwrap();
            fs::write(&kept, ".ROBLOSECURITY=_|WARNING:new\n").unwrap();
        };
        write();
        after_game(&p, "old", true);
        assert!(!p.join("roblox.cookie").exists());
        assert!(kept.exists() && p.join(PICKUP).exists());
        assert_eq!(renewed_cookie(&p, "old").as_deref(), Some("_|WARNING:new"));
        assert_eq!(renewed_cookie(&p, "_|WARNING:new"), None);
        // the next start of Alyae (resume, no game) saves it and cleans up
        after_game(&p, "old", false);
        assert!(!kept.exists() && !p.join(PICKUP).exists());
        write();
        after_game(&p, "old", false);
        assert!(!kept.exists() && !p.join("roblox.cookie").exists());
        let _ = fs::remove_dir_all(p);
    }

    /// Close all takes every Mocktail game and none of Mocktail's helpers; Anti-AFK only
    /// Alyae's games, which name their nudge file.
    #[test]
    fn which_processes_are_games() {
        for game in ["mocktail", "mocktail_alyae_abc", "mocktail_XII", "mocktail_I"] {
            assert!(any_game(game), "{game}");
        }
        for other in [
            "mocktail_webview_helper",
            "mocktail_cookie_helper",
            "mocktail_updater",
            "mocktail_failure_dialog",
            "mocktail.provision",
            "mocktail_XII.provision",
            "mocktail_",
            "mocktail_xii",
            "alyae",
        ] {
            assert!(!any_game(other), "{other}");
        }
        // the new name and the one from before numbers are both Alyae's; the plain program is not
        for game in ["mocktail_alyae_abc", "mocktail_XLVI"] {
            assert!(alyae_game(game), "{game}");
        }
        assert!(!alyae_game("mocktail"));
    }

    /// The shift-lock reference comes from the newest payload that has it, else from the file
    /// put in the cursors folder by hand.
    #[test]
    fn shiftlock_reference() {
        let root = std::env::temp_dir().join(format!("alyae-lock-{}", std::process::id()));
        let (payloads, cursors) = (root.join("payloads"), root.join("cursors"));
        fs::create_dir_all(&cursors).unwrap();
        assert_eq!(lock_reference(&payloads, &cursors), None);
        let hand = cursors.join(".roblox_lock_reference.png");
        fs::write(&hand, "png").unwrap();
        assert_eq!(lock_reference(&payloads, &cursors), Some(hand));
        let icon = |v: &str| payloads.join(v).join("assets/content/textures/MouseLockedCursor.png");
        for v in ["1-old", "2-new"] {
            fs::create_dir_all(icon(v).parent().unwrap()).unwrap();
            fs::write(icon(v), v).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        // a newer payload without the icon does not count
        fs::create_dir_all(payloads.join("3-none/assets")).unwrap();
        assert_eq!(lock_reference(&payloads, &cursors), Some(icon("2-new")));
        let _ = fs::remove_dir_all(root);
    }

    /// The installed Mocktail is Alyae's current build only when build.sh's mark matches the
    /// module's patches.
    #[test]
    fn mocktail_build_is_checked() {
        let expected = env!("ALYAE_MOCKTAIL_BUILD");
        assert_eq!(build_warning(Some(&format!("{expected}\n")), expected), None);
        assert!(build_warning(Some("e636d75 0000000000000000\n"), expected).unwrap().contains("older"));
        assert!(build_warning(None, expected).unwrap().contains("not Alyae's build"));
        // build.sh's mark: the upstream commit and 16 hex digits
        let (commit, hash) = expected.split_once(' ').unwrap();
        assert_eq!((commit.len(), hash.len()), (40, 16));
    }

    /// A nudge file that cannot be written is a failure, not a game started before Anti-AFK.
    #[test]
    fn failed_nudge_is_a_failure() {
        assert_eq!(nudge_report(2, 0, &[]), Ok("2 games nudged".into()));
        assert_eq!(
            nudge_report(1, 1, &[]),
            Ok("1 games nudged, 1 started before Anti-AFK existed (restart them)".into())
        );
        assert_eq!(
            nudge_report(1, 0, &["/p/nudge: denied".into()]),
            Err("1 games nudged, 1 not: /p/nudge: denied".into())
        );
    }

    /// Only alts are held to CPUs; a main keeps every core.
    #[test]
    fn only_alts_are_pinned() {
        assert_eq!(affinity(false), None);
        if let Some(cpus) = affinity(true) {
            let n = std::thread::available_parallelism().unwrap().get();
            assert!(!cpus.is_empty() && cpus.len() < n);
        }
    }

    #[test]
    fn frame_cap_kept_in_step() {
        let dir = std::env::temp_dir().join(format!("alyae-rbx-{}", std::process::id()));
        engine_settings(&dir, Role::Alt, Some(6));
        let f = dir.join("data/files/appData/GlobalBasicSettings_13.xml");
        assert!(
            fs::read_to_string(&f)
                .unwrap()
                .contains("<int name=\"FramerateCap\">6</int>")
        );
        engine_settings(&dir, Role::Alt, Some(30));
        let t = fs::read_to_string(&f).unwrap();
        assert!(t.contains("<int name=\"FramerateCap\">30</int>") && !t.contains(">6<"));
        let _ = fs::remove_dir_all(dir);
    }
}
