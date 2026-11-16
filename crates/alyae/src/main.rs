#![cfg_attr(windows, windows_subsystem = "windows")]
//! Alyae: game accounts in one small window. Games come as modules (see PLAN.md).

mod aeam;
mod log;
mod look;
mod module;
mod paths;
mod store;
mod transfer;
mod ui;
mod vault;

fn main() -> eframe::Result {
    let dir = paths::data_dir();
    // a portable copy or a chosen folder has no older Ludae data next to it
    let moved = if std::env::var_os("ALYAE_DATA").is_some() || paths::portable_dir().is_some() {
        Ok(())
    } else {
        paths::move_old_data(&dir)
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("alyae: cannot create {}: {e}", dir.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    log::init(&dir);
    log::line("", "Alyae started");
    let opened = moved
        .and_then(|()| vault::Vault::open(&dir.join("vault.key")))
        .and_then(|v| store::Store::open(&dir.join("alyae.db"), v));
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Alyae")
            .with_app_id("alyae")
            .with_inner_size([1000.0, 580.0])
            .with_min_inner_size([700.0, 400.0])
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/alyae.png"))
                    .unwrap_or_default(),
            ),
        ..Default::default()
    };
    eframe::run_native(
        "Alyae",
        options,
        Box::new(move |cc| {
            let modules = module::load_all(&paths::module_dirs());
            Ok(Box::new(ui::App::new(&cc.egui_ctx, opened, dir, modules)))
        }),
    )
}
