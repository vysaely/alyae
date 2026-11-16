//! Where Alyae keeps its files: `~/.local/share/alyae` on Linux, `%APPDATA%\Alyae` on Windows,
//! or the `data` folder beside the program when it is portable (a file named `portable` beside
//! it: a folder to carry on a USB stick or keep on a PC where nothing is installed).

use std::path::{Path, PathBuf};

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ALYAE_DATA") {
        return PathBuf::from(d);
    }
    if let Some(d) = portable_dir() {
        return d;
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(if cfg!(windows) { "Alyae" } else { "alyae" })
}

/// The data folder of a portable copy: `data` beside the running program when a file named
/// `portable` is there too.
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    portable_dir_of(exe.parent()?)
}

fn portable_dir_of(bin: &Path) -> Option<PathBuf> {
    bin.join("portable").is_file().then(|| bin.join("data"))
}

/// Alyae was called Ludae: its data moves over on the first start, with the database and log
/// renamed. Nothing happens once Alyae has a database. Entries Alyae already has (the installed
/// modules) stay; the old folder is removed when it is empty. An error names what failed.
pub fn move_old_data(new: &Path) -> Result<(), String> {
    let Some(base) = new.parent() else {
        return Ok(());
    };
    let old = base.join(if cfg!(windows) { "Ludae" } else { "ludae" });
    if new.join("alyae.db").exists() || !old.join("ludae.db").exists() {
        return Ok(());
    }
    if old_running() {
        return Err(
            "Ludae is still open: close it, then start Alyae (its accounts move over)".into(),
        );
    }
    move_into(&old, new)
}

/// Every entry of `old` into `new` (ludae.* renamed alyae.*), keeping what `new` has already.
fn move_into(old: &Path, new: &Path) -> Result<(), String> {
    std::fs::create_dir_all(new).map_err(|e| format!("cannot create {}: {e}", new.display()))?;
    let entries =
        std::fs::read_dir(old).map_err(|e| format!("cannot read {}: {e}", old.display()))?;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let to = new.join(name.replacen("ludae.", "alyae.", 1));
        if to.exists() {
            continue;
        }
        std::fs::rename(e.path(), &to).map_err(|err| format!("cannot move {name}: {err}"))?;
    }
    let _ = std::fs::remove_dir(old);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portable_marker_keeps_the_data_beside_the_program() {
        let bin = std::env::temp_dir().join(format!("alyae-portable-{}", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        assert_eq!(portable_dir_of(&bin), None);
        std::fs::write(bin.join("portable"), "").unwrap();
        assert_eq!(portable_dir_of(&bin), Some(bin.join("data")));
        let _ = std::fs::remove_dir_all(bin);
    }

    #[test]
    fn old_data_moves_into_an_existing_folder() {
        let root = std::env::temp_dir().join(format!("alyae-move-{}", std::process::id()));
        let (old, new) = (root.join("ludae"), root.join("alyae"));
        std::fs::create_dir_all(old.join("games/roblox")).unwrap();
        std::fs::create_dir_all(new.join("modules")).unwrap();
        for f in ["ludae.db", "ludae.db-wal", "vault.key", "ludae.log"] {
            std::fs::write(old.join(f), f).unwrap();
        }
        std::fs::create_dir_all(old.join("modules")).unwrap();
        move_into(&old, &new).unwrap();
        for f in [
            "alyae.db",
            "alyae.db-wal",
            "vault.key",
            "alyae.log",
            "games/roblox",
            "modules",
        ] {
            assert!(new.join(f).exists(), "{f}");
        }
        assert_eq!(
            std::fs::read_to_string(new.join("alyae.db")).unwrap(),
            "ludae.db"
        );
        // what Alyae had already stays in the old folder
        let left: Vec<_> = std::fs::read_dir(&old)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["modules"]);
        let _ = std::fs::remove_dir_all(root);
    }
}
