#![cfg_attr(windows, windows_subsystem = "windows")]
//! Alyae: game accounts in one small window. Games come as modules (see PLAN.md).

mod log;
mod look;
mod module;
mod paths;
mod ram;
mod store;
mod transfer;
mod ui;
mod vault;

fn version_line(args: impl IntoIterator<Item = impl AsRef<str>>) -> Option<&'static str> {
    (args.into_iter().nth(1).as_ref().map(AsRef::as_ref) == Some("--version"))
        .then_some(concat!("alyae ", env!("CARGO_PKG_VERSION")))
}

fn main() -> eframe::Result {
    if let Some(v) = version_line(std::env::args()) {
        println!("{v}");
        return Ok(());
    }
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
            let mut app = ui::App::new(&cc.egui_ctx, opened, dir, modules);
            app.check_builds(true);
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_flag() {
        assert_eq!(
            super::version_line(["alyae", "--version"]),
            Some(concat!("alyae ", env!("CARGO_PKG_VERSION")))
        );
        assert_eq!(super::version_line(["alyae"]), None);
    }
}
