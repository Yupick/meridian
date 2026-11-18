//! Pasa el linker script específico del kernel a `rust-lld`.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let linker_script = manifest_dir.join("linker.ld");

    println!("cargo:rustc-link-arg=-T{}", linker_script.display());
    // El target x86_64-unknown-none puede producir un static-pie según la
    // versión de Rust. El bootloader carga una imagen ET_EXEC en una dirección
    // física fija, así que forzamos un ejecutable no-PIE.
    println!("cargo:rustc-link-arg=--no-pie");
    println!("cargo:rustc-link-arg=--gc-sections");
    println!("cargo:rerun-if-changed={}", linker_script.display());
    println!("cargo:rerun-if-changed=build.rs");
}
