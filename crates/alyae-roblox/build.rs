//! The Mocktail build this helper expects, as build.sh marks an install of it
//! (`lib/mocktail/alyae-mocktail-patch`: the upstream commit, then the first 16 hex digits of
//! the SHA-256 of every patch in name order): `ALYAE_MOCKTAIL_BUILD`.

use sha2::{Digest, Sha256};
use std::path::Path;

fn main() {
    let dir = Path::new("../../modules/roblox-linux/mocktail/patches");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut patches: Vec<_> = std::fs::read_dir(dir)
        .expect("the module's Mocktail patches")
        .map(|e| e.expect("a patch").path())
        .filter(|p| p.extension().is_some_and(|x| x == "patch"))
        .collect();
    patches.sort();
    let mut sha = Sha256::new();
    for p in &patches {
        println!("cargo:rerun-if-changed={}", p.display());
        sha.update(std::fs::read(p).expect("a patch"));
    }
    let commit = std::fs::read_to_string(dir.join("UPSTREAM_COMMIT")).expect("UPSTREAM_COMMIT");
    let hex: String = sha.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!(
        "cargo:rustc-env=ALYAE_MOCKTAIL_BUILD={} {}",
        commit.trim(),
        &hex[..16]
    );
}
