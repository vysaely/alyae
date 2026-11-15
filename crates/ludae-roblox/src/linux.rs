//! Roblox on Linux through Mocktail: one profile folder per account, a per-account copy of the
//! Mocktail program (its single-instance lock follows the program's path), a main or alt role.
//! The settings mirror what AEAM ran with; AEAM-only extras (chat overlay, anti-AFK, self-heal)
//! are left out.

use crate::{Join, Request, api, fail, join_from, say, status, wait_for_stop};
use serde_json::json;
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    let mut bins: Vec<PathBuf> = std::env::var_os("LUDAE_MOCKTAIL")
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

fn mkdir(p: &Path) {
    let _ = fs::create_dir_all(p);
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
}

/// Writes `text` to `path` (mode 0600) through a temporary file, so a reader never sees half.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("ludae-tmp");
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

/// The account's own copy of the Mocktail program (a hard link where it can be one): Mocktail
/// derives its single-instance lock from its own path, so each account needs a path of its own.
fn account_program(m: &Mocktail, req: &Request, data_dir: &Path) -> PathBuf {
    let name = format!("mocktail_ludae_{}", &req.id[..req.id.len().min(12)]);
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
                if (s.contains(".so") || s.starts_with("mocktail_"))
                    && !s.starts_with("mocktail_acc_")
                    && !s.starts_with("mocktail_ludae_")
                {
                    let dest = dir.join(&n);
                    if fs::symlink_metadata(&dest).is_err() {
                        let _ = symlink(e.path(), dest);
                    }
                }
            }
        }
    }
    let target = dir.join(name);
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
    let want = program
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    let Ok(procs) = fs::read_dir("/proc") else {
        return false;
    };
    procs.flatten().any(|p| {
        fs::read(p.path().join("cmdline")).is_ok_and(|c| {
            let argv0 = c.split(|b| *b == 0).next().unwrap_or(&[]);
            Path::new(std::ffi::OsStr::from_bytes(argv0)).file_name() == Some(want.as_os_str())
        })
    })
}
use std::os::unix::ffi::OsStrExt;

/// Logical CPUs for alts: the upper ~35% of physical cores (with their SMT siblings), so a busy
/// alt never shares a core with the main. (main, alt) sets; None when there is nothing to split.
fn cpu_sets() -> Option<(Vec<usize>, Vec<usize>)> {
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
    let alt: Vec<usize> = cores[cores.len() - alt_cores..]
        .iter()
        .flat_map(|(_, v)| v.clone())
        .collect();
    let main: Vec<usize> = (0..n).filter(|c| !alt.contains(c)).collect();
    Some((main, alt))
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

fn uri(j: &Join) -> Option<String> {
    let mut u = match (j.follow, j.place) {
        (Some(user), Some(p)) => format!("roblox://experiences/start?placeId={p}&userId={user}"),
        (Some(user), None) => format!("roblox://experiences/start?userId={user}"),
        (None, Some(p)) => format!("roblox://experiences/start?placeId={p}"),
        (None, None) => return None,
    };
    if j.follow.is_none() {
        if let Some(job) = &j.job {
            u += &format!("&gameInstanceId={job}");
        } else if let Some(code) = &j.link_code {
            u += &format!("&linkCode={code}");
        }
    }
    Some(u)
}

/// FastFlags: the low-cost set for alts; for a main, only what AEAM's main ran with by default.
fn fast_flags(main: bool) -> serde_json::Value {
    let mut f = if main {
        json!({
            "FStringRenderTextureBudgetByRam": "",
            "DFFlagTextureQualityOverrideEnabled": true,
            "DFIntTextureQualityOverride": 3,
            "DFIntDebugFRMQualityLevelOverride": 14,
            "FFlagDebugDisableDRS": true,
            "FFlagAutomaticDRS": false,
            "FFlagAutomaticDRSDisableHighDPIFramebufferHalving": true,
            "FFlagDebugGraphicsPreferOpenGL": true,
            "DFFlagEnableRequestAsyncCompression": false,
        })
    } else {
        json!({
            "FStringRenderTextureBudgetByRam": "",
            "DFFlagTextureQualityOverrideEnabled": true,
            "DFIntTextureQualityOverride": 1,
            "DFIntDebugFRMQualityLevelOverride": 1,
            "FFlagDebugDisableDRS": true,
            "FFlagAutomaticDRS": false,
            "FFlagAutomaticDRSDisableHighDPIFramebufferHalving": true,
            "DFFlagSkipHighResolutiontextureMipsOnLowMemoryDevices2": true,
            "DFFlagEnableRequestAsyncCompression": false,
        })
    };
    // Roblox keeps decoded animation clips on Windows but not in the Android build Mocktail runs
    f["FFlagAnimationClipMemCacheEnabled"] = json!(true);
    f["FFlagDebugDisableCrashReporting"] = json!(true);
    f["FStringRobloxAnalyticsURL"] = json!("");
    f
}

/// Keeps Roblox's own frame cap in step with the account's: its settings file is created with
/// the role's quality, then only FramerateCap is set again (and alts kept quiet and low).
fn engine_settings(profile: &Path, main: bool, fps_cap: u32) {
    let file = profile.join("data/files/appData/GlobalBasicSettings_13.xml");
    let (gfx, vol) = if main { (7, "1") } else { (1, "0") };
    let Ok(text) = fs::read_to_string(&file) else {
        let xml = format!(
            r#"<roblox xmlns:xmime="http://www.w3.org/2005/05/xmlmime" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="http://www.roblox.com/roblox.xsd" version="4">
	<External>null</External>
	<External>nil</External>
	<Item class="UserGameSettings" referent="RBX16e4b21d820447a6adf2f8b7d933dc0b">
		<Properties>
			<token name="ControlMode">1</token>
			<int name="FramerateCap">{fps_cap}</int>
			<bool name="Fullscreen">false</bool>
			<token name="GraphicsOptimizationMode">1</token>
			<int name="GraphicsQualityLevel">{gfx}</int>
			<float name="MasterVolume">{vol}</float>
			<bool name="MaxQualityEnabled">false</bool>
			<token name="SavedQualityLevel">{gfx}</token>
			<string name="Name">GameSettings</string>
		</Properties>
	</Item>
</roblox>"#
        );
        let _ = fs::create_dir_all(file.parent().unwrap_or(profile));
        let _ = fs::write(&file, xml);
        return;
    };
    let mut out = String::with_capacity(text.len());
    let mut has_cap = false;
    for line in text.lines() {
        let t = line.trim_start();
        let indent = &line[..line.len() - t.len()];
        if t.contains("name=\"FramerateCap\"") {
            has_cap = true;
            out += &format!("{indent}<int name=\"FramerateCap\">{fps_cap}</int>\n");
        } else if !main && t.starts_with("<float name=\"MasterVolume\">") {
            out += &format!("{indent}<float name=\"MasterVolume\">0</float>\n");
        } else if !main
            && (t.contains("name=\"GraphicsQualityLevel\"")
                || t.contains("name=\"SavedQualityLevel\""))
        {
            let tag = if t.starts_with("<token") {
                "token"
            } else {
                "int"
            };
            let name = if t.contains("Saved") {
                "SavedQualityLevel"
            } else {
                "GraphicsQualityLevel"
            };
            out += &format!("{indent}<{tag} name=\"{name}\">1</{tag}>\n");
        } else {
            out += line;
            out.push('\n');
        }
    }
    if !has_cap {
        out = out.replacen(
            "<Properties>",
            &format!("<Properties>\n\t\t\t<int name=\"FramerateCap\">{fps_cap}</int>"),
            1,
        );
    }
    if out.trim_end() != text.trim_end() {
        let _ = fs::write(&file, out);
    }
}

/// The account's folder: Ludae's own, or AEAM's when the account came from there (its in-game
/// settings and caches stay).
fn profile_dir(req: &Request, data_dir: &Path) -> PathBuf {
    let p = req.get("profile");
    if !p.is_empty() && Path::new(p).is_dir() {
        return PathBuf::from(p);
    }
    data_dir.join("profiles").join(&req.id)
}

fn prepare_profile(m: &Mocktail, data_dir: &Path, profile: &Path, main: bool, fps_limit: u32) {
    for sub in [
        "data/mocktail",
        "config/mocktail",
        "cache/mocktail",
        "state/mocktail",
    ] {
        mkdir(&profile.join(sub));
    }
    // the Roblox client (payloads) is shared; the chosen version is copied
    let payloads = m.data.join("payloads");
    link_dir(&payloads, &profile.join("data/mocktail/payloads"));
    link_dir(&payloads, &profile.join("data/payloads"));
    if let Ok(cur) = fs::read(m.data.join("current.json")) {
        let _ = fs::write(profile.join("data/current.json"), &cur);
        let _ = fs::write(profile.join("data/mocktail/current.json"), &cur);
    }
    // downloaded assets: alts share one cache (fewer downloads, no CDN rate limits); a main keeps
    // its own, as Roblox deletes files its own index evicts
    let shared_assets = data_dir.join("shared_rbx_storage");
    mkdir(&shared_assets);
    for dest in [
        profile.join("data/mocktail/rbx-storage"),
        profile.join("data/rbx-storage"),
    ] {
        let is_link = fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink());
        if main && is_link {
            let _ = fs::remove_file(&dest);
        }
        if main {
            mkdir(&dest);
        } else if fs::symlink_metadata(&dest).is_err() {
            link_dir(&shared_assets, &dest);
        }
    }
    let shaders = data_dir.join("shared_mesa_shader_cache");
    mkdir(&shaders);
    link_dir(&shaders, &profile.join("cache/mesa_shader_cache"));

    let (w, h) = if main { (1920, 1080) } else { (800, 600) };
    let config = format!(
        "version: 1\ndevice: pc-windows-11\ngraphics:\n  backend: opengl\n  frame_rate_limit: {fps_limit}\n  vsync: off\n\
         performance:\n  multithreaded_rendering: true\n  physics_worker_mode: {}\n  gamemode: {}\n\
         window:\n  width: {w}\n  height: {h}\n  high_dpi: false\nupdates:\n  automatic: false\n",
        if main { "throughput" } else { "auto" },
        if main { "auto" } else { "off" },
    );
    let window = json!({
        "fullscreen": main, "maximized": false, "schema_version": 1,
        "windowed": { "has_position": false, "height": h, "width": w },
    })
    .to_string();
    for (file, text) in [
        (profile.join("config/mocktail/config.yaml"), &config),
        (profile.join("config/config.yaml"), &config),
        (profile.join("state/window-state.json"), &window),
        (profile.join("state/mocktail/window-state.json"), &window),
    ] {
        if let Err(e) = write_private(&file, text) {
            fail(&format!("cannot write {}: {e}", file.display()));
        }
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

fn write_cookie(profile: &Path, cookie: &str, tracker: Option<&str>) -> Vec<PathBuf> {
    let mut line = format!(".ROBLOSECURITY={cookie}\n");
    if let Some(t) = tracker {
        line += &format!("RBXEventTrackerV2={t}\n");
    }
    let paths = vec![
        profile.join("roblox.cookie"),
        profile.join("data/mocktail/roblox.cookie"),
    ];
    for p in &paths {
        if let Err(e) = write_private(p, &line) {
            fail(&format!("cannot write the sign-in file: {e}"));
        }
    }
    paths
}

fn runtime_dir(req: &Request, profile: &Path) -> PathBuf {
    let host = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let dir = match &host {
        Some(h) if h.is_dir() => h.join("ludae").join(&req.id),
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

pub fn play(req: &Request) {
    let cookie = req.cookie();
    if cookie.is_empty() {
        fail("no cookie: paste the .ROBLOSECURITY value into Cookie");
    }
    let Some(m) = find_mocktail() else {
        fail(
            "Mocktail is not installed (looked in ~/.local/lib/mocktail, /usr/lib/mocktail and PATH)",
        );
    };
    let data_dir = &req.data_dir;
    let main = req.is_main();
    let fps = req.fps();
    let fps_limit = if fps == 0 || fps >= 1000 { 10000 } else { fps };
    let program = account_program(&m, req, data_dir);
    if already_running(&program) {
        fail("this account is already running");
    }
    let join = join_from(req);
    let profile = profile_dir(req, data_dir);
    mkdir(&profile);
    status("starting");
    prepare_profile(
        &m,
        data_dir,
        &profile,
        main,
        if fps_limit == 10000 { 1000 } else { fps_limit },
    );
    engine_settings(&profile, main, fps_limit);
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
    let cookie_files = write_cookie(&profile, &cookie, tracker.as_deref());
    let run = runtime_dir(req, &profile);

    let (w, h) = if main {
        ("1920", "1080")
    } else {
        ("800", "600")
    };
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
        ("MOCKTAIL_DISABLE_FAILURE_DIALOG".into(), "1".into()),
        ("MOCKTAIL_GRAPHICS_BACKEND".into(), "opengl".into()),
        // a Windows PC with mouse and keyboard: Roblox shows its PC layout and settings
        ("MOCKTAIL_DEVICE_PROFILE".into(), "pc-windows-11".into()),
        ("MOCKTAIL_TOUCH_MODE".into(), "0".into()),
        ("MOCKTAIL_MOUSE_MODE".into(), "1".into()),
        ("MOCKTAIL_KEYBOARD_MODE".into(), "1".into()),
        ("MOCKTAIL_WIN_WIDTH".into(), w.into()),
        ("MOCKTAIL_WIN_HEIGHT".into(), h.into()),
        ("MOCKTAIL_WIN_HIGH_DPI".into(), "0".into()),
        (
            "MOCKTAIL_GRAPHICS_QUALITY".into(),
            if main { "manual".into() } else { "1".into() },
        ),
        ("vblank_mode".into(), "0".into()),
        ("MOCKTAIL_FRAME_RATE_LIMIT".into(), fps_limit.to_string()),
        ("MOCKTAIL_VSYNC".into(), "off".into()),
        ("SDL_VIDEO_WAYLAND_ALLOW_TEARING".into(), "1".into()),
        ("MOCKTAIL_SWAPCHAIN_IMAGES".into(), "3".into()),
        ("MOCKTAIL_MULTITHREADED_RENDERING".into(), "1".into()),
        (
            "MOCKTAIL_PHYSICS_WORKER_MODE".into(),
            if main {
                "throughput".into()
            } else {
                "auto".into()
            },
        ),
        (
            "MOCKTAIL_F11_FULLSCREEN".into(),
            if main { "1".into() } else { "0".into() },
        ),
        (
            "MOCKTAIL_FULLSCREEN_READINESS".into(),
            if main { "1".into() } else { "0".into() },
        ),
    ];
    let flags = fast_flags(main).to_string();
    env.push((
        "MOCKTAIL_CLIENT_SETTINGS_OVERRIDES_JSON".into(),
        flags.clone(),
    ));
    env.push(("MOCKTAIL_FAST_FLAGS_JSON".into(), flags));
    // Ludae's Mocktail build (anti-AFK patch): a nudge after this many seconds without input
    let afk_minutes: u64 = req.get("anti_afk").parse().unwrap_or(0);
    if afk_minutes > 0 {
        env.push((
            "MOCKTAIL_ANTI_AFK_SECONDS".into(),
            (afk_minutes.min(19) * 60).to_string(),
        ));
    }
    if !main {
        env.push(("SDL_AUDIODRIVER".into(), "dummy".into()));
        env.push(("PULSE_SERVER".into(), "/dev/null".into()));
        env.push(("ALSA_CARD".into(), "Dummy".into()));
        env.push(("MOCKTAIL_ENGINE_PUMP_REST".into(), "sleep".into()));
    }
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

    let sets = cpu_sets();
    let mut cmd = if !main && on_path("systemd-run") {
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
        if let Some((_, alt)) = &sets {
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
    let log_path = profile.join("ludae.log");
    if fs::metadata(&log_path).is_ok_and(|m| m.len() > 8 << 20) {
        let _ = fs::rename(&log_path, profile.join("ludae.log.1"));
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
        format!("\n=== Ludae launch {} ===\n", std::process::id()).as_bytes(),
    );
    cmd.stdin(Stdio::null())
        .stdout(log.try_clone().unwrap_or_else(|e| fail(&e.to_string())))
        .stderr(log);
    let affinity = sets.map(|(m, a)| if main { m } else { a });
    // SAFETY: only async-signal-safe calls between fork and exec
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            if let Some(cpus) = &affinity {
                set_affinity(cpus);
            }
            if !main {
                // no realtime or raised priority for alts, and a low share of the CPU
                let zero = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                libc::setrlimit(libc::RLIMIT_RTPRIO, &zero);
                libc::setrlimit(libc::RLIMIT_NICE, &zero);
                libc::nice(15);
            }
            Ok(())
        });
    }
    let started = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| fail(&format!("cannot start Mocktail: {e}")));
    let pid = child.id() as i32;
    say(json!({ "status": "running" }));

    // a stop from Ludae ends the game; Ludae closing leaves it running
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if wait_for_stop() {
            let _ = tx.send(());
        }
    });
    let code = loop {
        if let Ok(Some(s)) = child.try_wait() {
            break s;
        }
        if rx.try_recv().is_ok() {
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
    // Mocktail keeps the session in data/auth; Roblox may have renewed the cookie meanwhile
    let kept = profile.join("data/auth/roblox.cookie");
    if let Ok(text) = fs::read_to_string(&kept) {
        let renewed = api::clean_cookie(&text);
        if !renewed.is_empty() && renewed != cookie && renewed.starts_with("_|WARNING") {
            say(json!({ "set": { "cookie": renewed } }));
        }
    }
    for f in cookie_files.iter().chain([&kept]) {
        let _ = fs::remove_file(f);
    }
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
        let j = Join {
            place: None,
            job: None,
            link_code: None,
            follow: Some(9),
        };
        assert_eq!(
            uri(&j).as_deref(),
            Some("roblox://experiences/start?userId=9")
        );
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

    #[test]
    fn frame_cap_kept_in_step() {
        let dir = std::env::temp_dir().join(format!("ludae-rbx-{}", std::process::id()));
        engine_settings(&dir, false, 6);
        let f = dir.join("data/files/appData/GlobalBasicSettings_13.xml");
        assert!(
            fs::read_to_string(&f)
                .unwrap()
                .contains("<int name=\"FramerateCap\">6</int>")
        );
        engine_settings(&dir, false, 30);
        let t = fs::read_to_string(&f).unwrap();
        assert!(t.contains("<int name=\"FramerateCap\">30</int>") && !t.contains(">6<"));
        let _ = fs::remove_dir_all(dir);
    }
}
