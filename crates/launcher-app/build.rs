//! Embeds the icon and the version information into the executable.
//!
//! Without this the file manager and the task bar show a generic icon and
//! no version, which makes an installed application look unfinished.

fn main() {
    println!("cargo:rerun-if-changed=resources/app.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if !cfg!(target_os = "windows") {
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set_icon("resources/app.ico");
    res.set("FileDescription", "IDEAS Launcher — Rust 快速启动器");
    res.set("ProductName", "IDEAS Launcher");
    res.set("CompanyName", "IDEAS");
    res.set("LegalCopyright", "MIT License");
    res.set("FileVersion", env!("CARGO_PKG_VERSION"));
    res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
    res.set("InternalName", "IdeasLauncher");
    res.set("OriginalFilename", "IdeasLauncher.exe");

    if let Err(e) = res.compile() {
        // A missing icon must not stop the build; the application still works.
        println!("cargo:warning=could not embed the icon: {e}");
    }
}
