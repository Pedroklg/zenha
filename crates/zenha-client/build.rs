//! Embeds the icon and version metadata into the Windows executable.

use std::env;
use std::io;

fn main() -> io::Result<()> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }

    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=assets/zenha.ico");
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-changed=../../Cargo.toml");

    let version = env!("CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/zenha.ico")
        .set("ProductName", "Zenha")
        .set("FileDescription", "Zenha")
        .set("OriginalFilename", "zenha.exe")
        .set("ProductVersion", version)
        .set("FileVersion", version);
    res.compile()
}
