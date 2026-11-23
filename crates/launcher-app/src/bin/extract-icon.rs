//! Extract a Windows icon into a PNG, for debugging the icon pipeline.
//!
//! Run with: `cargo run --release -p ideas-launcher --bin extract-icon -- <exe> <out.png>`

use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: extract-icon <file.exe> <out.png>");
        std::process::exit(2);
    };
    let out = args.next().unwrap_or_else(|| "icon.png".to_string());

    let path = PathBuf::from(&input);
    match ideas_launcher::icons::extract_icon_to_png(&path, 32, PathBuf::from(&out).as_path()) {
        Some(()) => println!("wrote {out}"),
        None => {
            eprintln!("could not extract an icon from {input}");
            std::process::exit(1);
        }
    }
}
