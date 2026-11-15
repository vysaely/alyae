#![cfg_attr(windows, windows_subsystem = "windows")]
//! Ludae: game accounts in one small window. Games come as modules (see PLAN.md).

mod aeam;
mod log;
mod look;
mod module;
mod paths;
mod store;
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
    log::init(&dir);
    log::line("", "Ludae started");
    let opened = vault::Vault::open(&dir.join("vault.key"))
        .and_then(|v| store::Store::open(&dir.join("ludae.db"), v));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Ludae")
            .with_app_id("ludae")
            .with_inner_size([1000.0, 580.0])
            .with_min_inner_size([700.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Ludae",
        options,
        Box::new(move |cc| {
            let modules = module::load_all(&paths::module_dirs());
            Ok(Box::new(ui::App::new(&cc.egui_ctx, opened, dir, modules)))
        }),
    )
}
