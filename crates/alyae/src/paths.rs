//! Where Alyae keeps its files: `~/.local/share/alyae` on Linux, `%APPDATA%\Alyae` on Windows.

use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ALYAE_DATA") {
        return PathBuf::from(d);
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(if cfg!(windows) { "Alyae" } else { "alyae" })
}

/// Alyae was called Ludae: its data folder moves over on the first start, with the database and
/// log renamed. Nothing happens when the new folder exists already. An error names what failed.
pub fn move_old_data(new: &Path) -> Result<(), String> {
    let Some(base) = new.parent() else {
        return Ok(());
    };
    let old = base.join(if cfg!(windows) { "Ludae" } else { "ludae" });
    if new.exists() || !old.is_dir() {
        return Ok(());
    }
    if old_running() {
        return Err(
            "Ludae is still open: close it, then start Alyae (its accounts move over)".into(),
        );
    }
    std::fs::rename(&old, new).map_err(|e| format!("cannot move {}: {e}", old.display()))?;
    for (from, to) in [
        ("ludae.db", "alyae.db"),
        ("ludae.db-wal", "alyae.db-wal"),
        ("ludae.db-shm", "alyae.db-shm"),
        ("ludae.log", "alyae.log"),
        ("ludae.log.1", "alyae.log.1"),
    ] {
        let f = new.join(from);
        if f.exists() {
            std::fs::rename(&f, new.join(to)).map_err(|e| format!("cannot rename {from}: {e}"))?;
        }
    }
    Ok(())
}

/// A program named `ludae` running (Linux; elsewhere the move just goes ahead).
fn old_running() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_dir("/proc").is_ok_and(|procs| {
            procs.flatten().any(|p| {
                std::fs::read_to_string(p.path().join("comm")).is_ok_and(|c| c.trim() == "ludae")
            })
        })
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// Module folders, lowest priority first: shipped beside the program (or in the source tree when
/// run from it), then the system share folder, then the user's own.
pub fn module_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin) = exe.parent()
    {
        out.push(bin.join("modules"));
        // target/{debug,release}/alyae -> the repository's modules/
        out.push(bin.join("../../modules"));
        // ~/.local/bin/alyae -> ~/.local/share/alyae/modules is the user's; /usr/bin -> /usr/share
        out.push(bin.join("../share/alyae/modules"));
    }
    out.push(data_dir().join("modules"));
    out
}
