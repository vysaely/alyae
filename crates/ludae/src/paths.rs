//! Where Ludae keeps its files: `~/.local/share/ludae` on Linux, `%APPDATA%\Ludae` on Windows.

use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("LUDAE_DATA") {
        return PathBuf::from(d);
    }
    let base = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(if cfg!(windows) { "Ludae" } else { "ludae" })
}

/// Module folders, lowest priority first: shipped beside the program (or in the source tree when
/// run from it), then the system share folder, then the user's own.
pub fn module_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(bin) = exe.parent()
    {
        out.push(bin.join("modules"));
        // target/{debug,release}/ludae -> the repository's modules/
        out.push(bin.join("../../modules"));
        // ~/.local/bin/ludae -> ~/.local/share/ludae/modules is the user's; /usr/bin -> /usr/share
        out.push(bin.join("../share/ludae/modules"));
    }
    out.push(data_dir().join("modules"));
    out
}
