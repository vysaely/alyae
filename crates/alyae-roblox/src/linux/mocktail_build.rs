//! Alyae's Build Mocktail button: the module's `mocktail/build.sh` builds Mocktail with Alyae's
//! patches at low priority, its output in Alyae's log (this helper's stderr) and its progress in
//! status lines. The install replaces the program the games run from, so it waits until no game
//! runs.
//!
//! `mocktail_check` (when Alyae opens, and after a build): `{"warn": text, "built": bool}` when
//! the installed Mocktail is not the build this helper expects, nothing when it is; `built`: the
//! build is finished and only its install is left. `mocktail_build` gets `{"module_dir"}`; a
//! `stop` line ends it.

use super::{any_game, build_warning, find_mocktail, home, running_games};
use crate::{Stop, fail, say, status};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

const EXPECTED: &str = env!("ALYAE_MOCKTAIL_BUILD");
/// build.sh's mark, beside the installed program and in its build folder
const MARK: &str = "alyae-mocktail-patch";
const WAITING: &str = "Mocktail is built: it installs when the games are closed";

/// What a Build Mocktail run has left to do.
#[derive(Debug, PartialEq)]
enum Step {
    Nothing,
    Build,
    /// built; the install waits for the games to close
    Wait,
    Install,
}

/// From the installed mark, the finished build's mark and whether a game runs.
fn step(installed: Option<&str>, built: Option<&str>, expected: &str, games: bool) -> Step {
    let is = |m: Option<&str>| m.map(str::trim) == Some(expected);
    if is(installed) {
        Step::Nothing
    } else if !is(built) {
        Step::Build
    } else if games {
        Step::Wait
    } else {
        Step::Install
    }
}

/// The installed Mocktail (None: none is) and its mark.
fn installed() -> Option<Option<String>> {
    find_mocktail().map(|m| fs::read_to_string(m.lib.join(MARK)).ok())
}

/// The mark of build.sh's finished build, in its build folder: the source checkout
/// (`ALYAE_MOCKTAIL_SRC`, else ~/.cache/alyae/mocktail-src) with `-build` added.
fn built() -> Option<String> {
    let mut dir = std::env::var_os("ALYAE_MOCKTAIL_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache/alyae/mocktail-src"))
        .into_os_string();
    dir.push("-build");
    fs::read_to_string(Path::new(&dir).join(MARK)).ok()
}

fn games_running() -> bool {
    !running_games(any_game).is_empty()
}

pub fn check() {
    let installed = installed();
    let mark = installed.clone().flatten();
    let Some(warning) = build_warning(mark.as_deref(), EXPECTED) else {
        return;
    };
    let step = step(
        mark.as_deref(),
        built().as_deref(),
        EXPECTED,
        games_running(),
    );
    let text = match (&step, &installed) {
        (Step::Wait, _) => WAITING,
        (_, None) => "Mocktail is not installed: Join Server needs it",
        _ => warning,
    };
    say(json!({ "warn": text, "built": step != Step::Build }));
}

pub fn build(v: &Value) {
    let Some(dir) = v["module_dir"].as_str() else {
        fail("no module_dir in the request");
    };
    let script = Path::new(dir).join("mocktail/build.sh");
    if !script.is_file() {
        fail(&format!(
            "{} is missing: install Alyae again with install.sh",
            script.display()
        ));
    }
    let mark = || installed().flatten();
    let mut stop = Stop::listen();
    match step(
        mark().as_deref(),
        built().as_deref(),
        EXPECTED,
        games_running(),
    ) {
        Step::Nothing => {
            status("Building Mocktail: done");
            return;
        }
        Step::Build => {
            run(&script, "--build", &mut stop);
            if built().as_deref().map(str::trim) != Some(EXPECTED) {
                fail("build.sh ended without marking its build: see Alyae's log");
            }
        }
        Step::Wait | Step::Install => {}
    }
    let mut said = false;
    while games_running() {
        if !said {
            status(WAITING);
            said = true;
        }
        if stop.asked() || stop.alyae_gone {
            status("Building Mocktail: stopped; the build is kept and installs when Alyae opens");
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    status("Building Mocktail: installing");
    run(&script, "--install", &mut stop);
    if mark().as_deref().map(str::trim) != Some(EXPECTED) {
        fail("build.sh installed Mocktail but Alyae does not find its mark: see Alyae's log");
    }
    status("Building Mocktail: done");
}

/// The build's jobs: half the cores, at least 2, so the desktop and the running games keep the
/// rest.
fn build_jobs(cores: usize) -> usize {
    (cores / 2).max(2)
}

/// One step of build.sh, at the lowest CPU and disk priority, in a process group of its own so
/// Cancel ends the compilers too. Its lines go to Alyae's log; what it is doing to status lines.
fn run(script: &Path, flag: &str, stop: &mut Stop) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut child = Command::new("nice")
        .args(["-n", "19", "ionice", "-c3", "bash"])
        .arg(script)
        .arg(flag)
        .env("CMAKE_BUILD_PARALLEL_LEVEL", build_jobs(cores).to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| fail(&format!("cannot run {}: {e}", script.display())));
    let out = child.stdout.take().expect("piped stdout");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut said = String::new();
    let mut show = |text: String| {
        if text != said {
            status(&text);
            said = text;
        }
    };
    let (mut log, mut last, mut missing) = (None, String::new(), None);
    loop {
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(line) => {
                eprintln!("build.sh {flag}: {line}");
                match stage(&line) {
                    Some(Stage::Fetching) => show("Building Mocktail: fetching".into()),
                    Some(Stage::Patching) => show("Building Mocktail: applying patches".into()),
                    Some(Stage::Building(p)) => {
                        show("Building Mocktail: building".into());
                        log = Some(p);
                    }
                    None => {}
                }
                if line.starts_with("Missing build tools") {
                    missing = Some(line.clone());
                }
                last = line;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if stop.asked() || stop.alyae_gone {
            // SAFETY: a signal to the process group this function started
            unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
            let _ = child.wait();
            status("Building Mocktail: cancelled");
            std::process::exit(0);
        }
        if let Some(p) = log.as_deref().and_then(|l| progress(&log_tail(l))) {
            show(format!("Building Mocktail: building {p}%"));
        }
    }
    match child.wait() {
        Ok(s) if s.success() => {}
        Ok(s) => fail(&missing.unwrap_or_else(|| {
            format!("build.sh {flag} failed ({s}): {last} (all of it in Alyae's log)")
        })),
        Err(e) => fail(&format!("build.sh {flag}: {e}")),
    }
}

/// What build.sh says it is doing.
#[derive(Debug, PartialEq)]
enum Stage {
    Fetching,
    Patching,
    /// the compiler's output goes to this log
    Building(PathBuf),
}

fn stage(line: &str) -> Option<Stage> {
    if line.starts_with("Mocktail ") && line.ends_with(": fetching") {
        Some(Stage::Fetching)
    } else if line.starts_with("  applied ") {
        Some(Stage::Patching)
    } else {
        let (_, log) = line.strip_prefix("Building (")?.split_once("log: ")?;
        Some(Stage::Building(log.strip_suffix(')')?.into()))
    }
}

/// The build's percentage from ninja's last `[done/total]` line in `log`.
fn progress(log: &str) -> Option<u64> {
    log.lines().rev().find_map(|l| {
        let (done, total) = l.strip_prefix('[')?.split_once(']')?.0.split_once('/')?;
        let (done, total) = (done.parse::<u64>().ok()?, total.parse::<u64>().ok()?);
        (total > 0).then(|| done * 100 / total)
    })
}

/// The end of the build log; empty while it cannot be read (the percentage then waits).
fn log_tail(path: &Path) -> String {
    let mut b = Vec::new();
    if let Ok(mut f) = fs::File::open(path) {
        let len = f.metadata().map_or(0, |m| m.len());
        if f.seek(SeekFrom::Start(len.saturating_sub(8192))).is_ok() {
            let _ = f.read_to_end(&mut b);
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// build.sh's own lines become the stages Alyae shows.
    #[test]
    fn stages_from_build_sh() {
        assert_eq!(stage("Mocktail e636d75: fetching"), Some(Stage::Fetching));
        assert_eq!(stage("  applied 01-fps-uncap.patch"), Some(Stage::Patching));
        assert_eq!(
            stage("Building (a few minutes; log: /h/.cache/alyae/mocktail-src-build.log)"),
            Some(Stage::Building(
                "/h/.cache/alyae/mocktail-src-build.log".into()
            ))
        );
        assert_eq!(
            stage("Previous Mocktail kept in /h/.local/lib/mocktail-backup/x"),
            None
        );
    }

    /// The lines `stage` and `run` read are the ones build.sh prints.
    #[test]
    fn build_sh_says_them() {
        let sh = include_str!("../../../../modules/roblox-linux/mocktail/build.sh");
        for said in [
            r#"echo "Mocktail ${UPSTREAM_COMMIT:0:7}: fetching""#,
            r#"echo "  applied $(basename "$p")""#,
            r#"echo "Building (a few minutes; log: $BUILD.log)""#,
            r#"echo "Missing build tools: "#,
            r#"BUILT="$BUILD/alyae-mocktail-patch""#,
            r#"BUILD="$SRC-build""#,
            r#"SRC="${ALYAE_MOCKTAIL_SRC:-$HOME/.cache/alyae/mocktail-src}""#,
        ] {
            assert!(sh.contains(said), "build.sh no longer says {said}");
        }
    }

    /// The percentage comes from ninja's last progress line; other lines do not count.
    #[test]
    fn progress_from_ninja() {
        let log = "-- Configuring done\n[1/400] Building CXX object a.o\n\
                   [180/400] Building CXX object b.o\nwarning: unused variable\n";
        assert_eq!(progress(log), Some(45));
        assert_eq!(progress("-- Configuring done\n"), None);
        // the tail of a log can start in the middle of a line
        assert_eq!(progress("0/400] Building CXX object a.o\n"), None);
        assert_eq!(progress("[0/0] nothing\n"), None);
    }

    #[test]
    fn half_the_cores_at_least_two() {
        assert_eq!(build_jobs(1), 2);
        assert_eq!(build_jobs(4), 2);
        assert_eq!(build_jobs(16), 8);
        assert_eq!(build_jobs(7), 3);
    }

    /// A finished build waits while a game runs (it would replace the game's program) and
    /// installs once none does; an older finished build is built again.
    #[test]
    fn install_waits_for_the_games() {
        let want = "e636d75 0123456789abcdef";
        let old = "e636d75 0000000000000000";
        assert_eq!(
            step(Some(&format!("{want}\n")), None, want, true),
            Step::Nothing
        );
        assert_eq!(step(Some(old), Some(old), want, false), Step::Build);
        assert_eq!(step(None, None, want, false), Step::Build);
        assert_eq!(step(Some(old), Some(want), want, true), Step::Wait);
        assert_eq!(
            step(None, Some(&format!("{want}\n")), want, false),
            Step::Install
        );
    }
}
