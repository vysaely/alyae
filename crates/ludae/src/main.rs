#![cfg_attr(windows, windows_subsystem = "windows")]
//! Ludae: game accounts in one small window. Games come as modules (see PLAN.md).

mod aeam;
mod look;
mod module;
mod paths;
mod store;
mod totp;
mod ui;
mod vault;

fn main() -> eframe::Result {
    let dir = paths::data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("ludae: cannot create {}: {e}", dir.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let opened = vault::Vault::open(&dir.join("vault.key"))
        .and_then(|v| store::Store::open(&dir.join("ludae.db"), v));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Ludae")
            .with_app_id("ludae")
            .with_inner_size([860.0, 560.0])
            .with_min_inner_size([520.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Ludae",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, opened, dir)))),
    )
}
