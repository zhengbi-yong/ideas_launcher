//! Compare the ways of getting the icon of a folder.
//!
//! Run with:
//! `cargo run --release -p ideas-launcher --bin folder-icon-probe -- "C:\Program Files\Clash Verge" <out-dir>`
//!
//! Every way is written to its own PNG, so they can be looked at side by
//! side: the shell knows several ways of answering, and only one of them is
//! the folder icon the file manager draws.

use ideas_launcher::icons::shell_icon;
use ideas_launcher::winapi::*;
use std::path::{Path, PathBuf};

fn save(image: &ideas_launcher::icons::IconImage, out: &Path) {
    let Some(rgba) = image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.rgba.clone()) else {
        eprintln!("bad image");
        return;
    };
    match rgba.save(out) {
        Ok(()) => println!("  -> {}", out.display()),
        Err(error) => eprintln!("  -> {} failed: {error}", out.display()),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: folder-icon-probe <folder> <out-dir>");
        std::process::exit(2);
    };
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".to_string()));
    let path = PathBuf::from(&input);

    let mut with_backslash = path.as_os_str().to_os_string();
    with_backslash.push("\\");
    let with_backslash = PathBuf::from(with_backslash);

    println!("folder: {}", path.display());

    println!("1. SHGFI_ICON | SHGFI_USEFILEATTRIBUTES, with a trailing backslash");
    if let Some(image) = shell_icon(&with_backslash, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES, 32) {
        save(&image, &out_dir.join("1-attributes-backslash.png"));
    } else {
        println!("  -> nothing");
    }

    println!("2. SHGFI_ICON | SHGFI_USEFILEATTRIBUTES, the path as it is");
    if let Some(image) = shell_icon(&path, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES, 32) {
        save(&image, &out_dir.join("2-attributes.png"));
    } else {
        println!("  -> nothing");
    }

    println!("3. SHGFI_ICON, the file system is asked");
    if let Some(image) = shell_icon(&path, SHGFI_ICON, 32) {
        save(&image, &out_dir.join("3-disk.png"));
    } else {
        println!("  -> nothing");
    }

    println!("4. SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES, with a trailing backslash");
    if let Some(found) = sys_icon_index(&with_backslash, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES) {
        println!("  -> index {found}");
    } else {
        println!("  -> no index");
    }

    println!("5. SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES, the path as it is");
    if let Some(found) = sys_icon_index(&path, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES) {
        println!("  -> index {found}");
    } else {
        println!("  -> no index");
    }

    println!("6. SHGFI_SYSICONINDEX, the file system is asked");
    if let Some(found) = sys_icon_index(&path, SHGFI_SYSICONINDEX) {
        println!("  -> index {found}");
    } else {
        println!("  -> no index");
    }
}

/// The index of the icon in the system image list.
fn sys_icon_index(path: &Path, flags: u32) -> Option<i32> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let mut info: SHFILEINFO = std::mem::zeroed();
        let ok = SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFO>() as u32,
            flags,
        );
        if ok == 0 {
            return None;
        }
        Some(info.iIcon)
    }
}
