//! Removing files and counting what it frees, for Reset files and forgetting an account.

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
}
