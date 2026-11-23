//! Check the shortcut parser against the shell.
//!
//! Run with:
//! `cargo run --release -p ideas-launcher --bin lnk-dump -- <folder> [<folder> ...]`
//!
//! Every `.lnk` below the folders is parsed and printed as
//! `path<TAB>target`. The shell is the referee: `WScript.Shell` resolves the
//! same shortcuts, and the two answers have to agree.

use std::path::{Path, PathBuf};

fn walk(folder: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase() == "lnk")
            .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

fn main() {
    let folders: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let mut shortcuts = Vec::new();
    for folder in &folders {
        walk(folder, &mut shortcuts);
    }
    shortcuts.sort();

    for path in &shortcuts {
        let target = std::fs::read(path)
            .ok()
            .and_then(|data| launcher_index::lnk::parse(&data))
            .and_then(|link| link.target_path);
        // A target that is relative belongs to the folder of the shortcut,
        // and a target that is not there is worse than none: the shell
        // resolves the shortcut itself. This mirrors what the launcher does.
        let target = match target {
            Some(t) if !Path::new(&t).is_absolute() => match path.parent() {
                Some(folder) => {
                    Some(launcher_index::lnk::resolve_relative(folder, Path::new(&t)))
                }
                None => Some(PathBuf::from(t)),
            },
            other => other.map(PathBuf::from),
        };
        let target = target
            .filter(|t| t.exists())
            .unwrap_or_else(|| path.clone());
        println!("{}\t{}", path.display(), target.to_string_lossy());
    }
}
