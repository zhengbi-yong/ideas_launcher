//! Where the launcher keeps its data.
//!
//! The layout mirrors Flow Launcher's `%APPDATA%\FlowLauncher`, so an
//! existing configuration can be pointed at directly.

use std::path::{Path, PathBuf};

/// The root of the launcher's data.
pub fn data_directory() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("IdeasLauncher")
}

/// The directory the launcher used to keep its data in.
///
/// The program was called *Fast Launcher* before it was renamed to
/// *IDEAS Launcher*. Renaming the directory on its own would throw the
/// user's history and settings away, so the old files are carried over.
const LEGACY_DIRECTORY: &str = "FastLauncher";

/// The settings directory.
pub fn settings_directory() -> PathBuf {
    data_directory().join("Settings")
}

/// The settings file.
pub fn settings_file() -> PathBuf {
    settings_directory().join("Settings.json")
}

/// The query history file.
pub fn history_file() -> PathBuf {
    settings_directory().join("History.json")
}

/// The selection-memory file.
pub fn selection_file() -> PathBuf {
    settings_directory().join("UserSelectedRecord.json")
}

/// The cache directory.
pub fn cache_directory() -> PathBuf {
    data_directory().join("Cache")
}

/// The log directory.
pub fn log_directory() -> PathBuf {
    data_directory().join("Logs")
}

/// The log file of the current day.
///
/// A window program has no console, so the log has to go to a file.
pub fn log_file() -> PathBuf {
    log_directory().join(format!("ideas-launcher-{}.log", today()))
}

/// The file the performance measurements are written to.
///
/// It only exists while the measuring is on — see the `LAUNCHER_PROFILE`
/// environment variable.
pub fn profile_report_file() -> PathBuf {
    data_directory().join("profile-report.txt")
}

/// The date of today, as `YYYY-MM-DD`.
fn today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (year, month, day) = civil_from_unix(now);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The civil date from a Unix timestamp (Howard Hinnant's algorithm).
fn civil_from_unix(unix: i64) -> (i64, i64, i64) {
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The per-plugin settings directory.
pub fn plugin_settings_directory(plugin_id: &str) -> PathBuf {
    settings_directory().join("Plugins").join(plugin_id)
}

/// The directory the user's own plugins live in.
///
/// Every plugin sits in its own directory below it and announces itself with
/// a `plugin.json`.
pub fn plugins_directory() -> PathBuf {
    data_directory().join("Plugins")
}

/// Create the directories the launcher needs.
pub fn ensure_directories() {
    // The carry-over has to happen first: it only runs while the new
    // directory is still empty.
    migrate_legacy_directory();
    for dir in [
        settings_directory(),
        cache_directory(),
        log_directory(),
    ] {
        let _ = std::fs::create_dir_all(&dir);
    }
}

/// Carry the data of the old installation over to the new directory.
///
/// The launcher was called *Fast Launcher* before it was renamed. The files
/// are copied, not moved: the old directory is left alone, so going back to
/// the old version still finds its history. A file that already exists in
/// the new directory is never overwritten — the new one is the newer one.
pub fn migrate_legacy_directory() {
    let config = match dirs::config_dir() {
        Some(dir) => dir,
        None => return,
    };
    let old = config.join(LEGACY_DIRECTORY);
    let new = data_directory();
    if !old.is_dir() {
        return;
    }
    match copy_missing(&old, &new) {
        Ok(count) if count > 0 => {
            log::info!("{count} files were carried over from the old installation");
        }
        Ok(_) => {}
        Err(e) => log::warn!("the old data could not be carried over: {e}"),
    }
}

/// Copy everything from `from` to `to` that is not there yet.
///
/// Returns how many files were copied.
fn copy_missing(from: &Path, to: &Path) -> std::io::Result<usize> {
    let mut copied = 0;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copied += copy_missing(&entry.path(), &target)?;
        } else if !target.exists() {
            std::fs::create_dir_all(
                target.parent().unwrap_or_else(|| Path::new(".")),
            )?;
            std::fs::copy(entry.path(), &target)?;
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_under_the_data_directory() {
        let root = data_directory();
        for path in [
            settings_file(),
            history_file(),
            selection_file(),
            cache_directory(),
            log_directory(),
        ] {
            assert!(path.starts_with(&root), "{path:?}");
        }
    }

    #[test]
    fn plugin_settings_are_isolated_per_plugin() {
        let a = plugin_settings_directory("one");
        let b = plugin_settings_directory("two");
        assert_ne!(a, b);
        assert!(a.ends_with("one"));
    }

    #[test]
    fn the_log_file_is_under_the_log_directory() {
        let file = log_file();
        assert!(file.starts_with(log_directory()));
        assert!(file.to_string_lossy().ends_with(".log"));
    }

    #[test]
    fn the_date_is_formatted_as_iso() {
        let (year, month, day) = civil_from_unix(1_762_000_000);
        assert_eq!((year, month, day), (2025, 11, 1));
        // A day later.
        let (year, month, day) = civil_from_unix(1_762_000_000 + 86_400);
        assert_eq!((year, month, day), (2025, 11, 2));
    }
}
