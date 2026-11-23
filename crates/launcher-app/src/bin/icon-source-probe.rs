//! Dump every way of getting the icon of a path, side by side.
//!
//! Run with:
//! `cargo run --release -p ideas-launcher --bin icon-source-probe -- <path> <out-dir>`
//!
//! The launcher asks for an icon in four steps, and this tool prints what
//! each of them answers: the icon inside the file, the icon of the program
//! that opens it (without touching the disk), the same one with the disk
//! (which is also how folders are asked), and the picture itself.

use ideas_launcher::icons::{extract_icon, picture_icon, shell_icon};
use ideas_launcher::winapi::*;
use std::path::{Path, PathBuf};

fn save(image: &ideas_launcher::icons::IconImage, out: &Path) {
    let Some(rgba) =
        image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.rgba.clone())
    else {
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
        eprintln!("usage: icon-source-probe <path> <out-dir>");
        std::process::exit(2);
    };
    let out_dir = PathBuf::from(args.next().unwrap_or_else(|| ".".to_string()));
    let path = PathBuf::from(&input);
    let stem = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "icon".to_string());
    let stem = stem.replace(['.', ' '], "_");

    println!("path: {}", path.display());
    println!(
        "is_directory: {}",
        std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false)
    );

    println!("1. extract_icon — the icon inside the file (ExtractIconEx)");
    if let Some(image) = extract_icon(&path, 32) {
        save(&image, &out_dir.join(format!("{stem}-1-inside.png")));
    } else {
        println!("  -> nothing");
    }

    println!("2. picture_icon — the picture itself");
    if let Some(image) = picture_icon(&path) {
        save(&image, &out_dir.join(format!("{stem}-2-picture.png")));
    } else {
        println!("  -> nothing");
    }

    println!("3. shell, without touching the disk (SHGFI_USEFILEATTRIBUTES)");
    if let Some(image) = shell_icon(&path, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES, 32) {
        save(&image, &out_dir.join(format!("{stem}-3-attributes.png")));
    } else {
        println!("  -> nothing");
    }

    println!("4. shell, asking the file system (SHGFI_ICON)");
    if let Some(image) = shell_icon(&path, SHGFI_ICON, 32) {
        save(&image, &out_dir.join(format!("{stem}-4-disk.png")));
    } else {
        println!("  -> nothing");
    }
}
