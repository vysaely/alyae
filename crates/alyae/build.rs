//! Puts Alyae's icon into alyae.exe on Windows; elsewhere the window icon (main.rs) and the
//! launcher entry (install.sh) carry it.

fn main() {
    println!("cargo:rerun-if-changed=alyae.rc");
    println!("cargo:rerun-if-changed=../../assets/alyae.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("alyae.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("cannot compile alyae.rc (the icon)");
    }
}
