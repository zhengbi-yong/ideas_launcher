//! Try the details of a file.
//!
//! Run with:
//! `cargo run --release -p ideas-launcher --bin details-probe -- <path> [<path> ...]`

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let paths: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if paths.is_empty() {
        eprintln!("usage: details-probe <path> [<path> ...]");
        std::process::exit(2);
    }
    for path in paths {
        let started = Instant::now();
        let details = ideas_launcher::details::collect(&path);
        let took = started.elapsed();
        println!("== {} ({took:?})", path.display());
        for (title, rows) in details.groups() {
            println!("  [{title}]");
            for (label, value) in rows {
                println!("    {label:<12} {value}");
            }
        }
        println!();
    }
}
