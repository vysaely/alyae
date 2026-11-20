//! Removing files and counting what it frees, for Reset files and forgetting an account; moving
//! an account's files from their old name (its id) to its numeral.

use std::fs;
use std::path::Path;

/// Size of `p` in bytes, links not followed.
pub(crate) fn size_of(p: &Path) -> u64 {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            fs::read_dir(p).map_or(0, |d| d.flatten().map(|e| size_of(&e.path())).sum())
        }
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Removes `p`; a link is removed itself, never what it points to (the shared client and caches).
pub(crate) fn remove(p: &Path) -> u64 {
    let size = size_of(p);
    let gone = match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(_) => return 0,
    };
    if gone.is_err() { 0 } else { size }
}

/// Removes everything in `dir` but the names in `keep`.
#[cfg(target_os = "linux")]
pub(crate) fn clear(dir: &Path, keep: &[&str]) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| !keep.contains(&e.file_name().to_string_lossy().as_ref()))
        .map(|e| remove(&e.path()))
        .sum()
}

/// `new`, or `old` while only that one is there: an account's file or folder named by its id, as
/// Alyae named them before accounts had numbers, until a launch renames it (`rename_old`).
pub(crate) fn current(old: &Path, new: &Path) -> std::path::PathBuf {
    if fs::symlink_metadata(new).is_err() && fs::symlink_metadata(old).is_ok() {
        old.to_path_buf()
    } else {
        new.to_path_buf()
    }
}

/// Renames `old` (named by the account id) to `new` (its numeral), refused while the account's
/// game runs, as the game has it open. Nothing at `old`, or `new` there already: nothing to do.
pub(crate) fn rename_old(old: &Path, new: &Path, running: bool) -> Result<(), String> {
    if current(old, new) == new {
        return Ok(());
    }
    if running {
        return Err(format!(
            "close this account's game first: {} gets its new name {} at this start",
            old.display(),
            new.display()
        ));
    }
    fs::rename(old, new)
        .map_err(|e| format!("cannot rename {} to {}: {e}", old.display(), new.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_counts_what_it_frees_and_a_missing_path_frees_nothing() {
        let dir = std::env::temp_dir().join(format!("alyae-files-{}", std::process::id()));
        fs::create_dir_all(dir.join("cache/sub")).unwrap();
        fs::write(dir.join("cache/a"), [0u8; 100]).unwrap();
        fs::write(dir.join("cache/sub/b"), [0u8; 50]).unwrap();
        assert_eq!(remove(&dir.join("cache")), 150);
        assert!(!dir.join("cache").exists());
        assert_eq!(remove(&dir.join("cache")), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder named by the account id is renamed to its numeral once, not while the game runs,
    /// and is found under either name meanwhile.
    #[test]
    fn old_folder_gets_its_numeral() {
        let dir = std::env::temp_dir().join(format!("alyae-rename-{}", std::process::id()));
        let (old, new) = (dir.join("0123abcd"), dir.join("XII"));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("browser_tracker"), "1").unwrap();
        assert_eq!(current(&old, &new), old);
        assert!(rename_old(&old, &new, true).unwrap_err().contains("close"));
        assert!(old.is_dir() && !new.exists());
        rename_old(&old, &new, false).unwrap();
        assert!(!old.exists());
        assert_eq!(
            fs::read_to_string(new.join("browser_tracker")).unwrap(),
            "1"
        );
        assert_eq!(current(&old, &new), new);
        // nothing left to rename: nothing to do, running or not
        rename_old(&old, &new, true).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
