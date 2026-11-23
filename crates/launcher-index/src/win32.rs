//! The Win32 program index.
//!
//! Six sources, walked in parallel, exactly mirroring the sources Flow
//! Launcher uses but without COM.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::lnk;

/// Where a program was discovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramSourceKind {
    /// A Start Menu entry.
    StartMenu,
    /// A directory on `%PATH%`.
    PathEnv,
    /// A registry `App Paths` entry.
    AppPaths,
    /// A registry `Uninstall` entry.
    Uninstall,
    /// A user configured directory.
    CustomDirectory,
}

impl ProgramSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProgramSourceKind::StartMenu => "start_menu",
            ProgramSourceKind::PathEnv => "path",
            ProgramSourceKind::AppPaths => "app_paths",
            ProgramSourceKind::Uninstall => "uninstall",
            ProgramSourceKind::CustomDirectory => "custom",
        }
    }
}

/// A single indexed program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Display name.
    pub name: String,
    /// Absolute path of the executable (or of the shortcut for `.lnk`).
    pub full_path: PathBuf,
    /// Path the icon should be taken from — usually the resolved target.
    pub icon_path: Option<PathBuf>,
    /// Description from the registry or the shortcut.
    pub description: String,
    /// Arguments recorded in the shortcut.
    pub args: Option<String>,
    /// Which source found this program.
    pub source: ProgramSourceKind,
    /// Disabled programs are still indexed but ranked lower.
    pub enabled: bool,
    /// Lower-cased `name + path + description`, pre-computed for searching.
    pub search_text: String,
    /// Which ASCII characters occur in `search_text`.
    ///
    /// A fuzzy match can only use characters that are actually present, so
    /// this mask rejects impossible candidates in constant time — the
    /// difference between scanning 2 700 paths per keystroke and scanning
    /// none.
    pub text_mask: u128,
    /// Which ASCII characters occur in the lower-cased display name.
    pub name_mask: u128,
    /// The pinyin rendering of the display name, when it contains CJK.
    ///
    /// It is built once, when the index is created, so that a pinyin query
    /// costs nothing extra at search time.
    pub pinyin: Option<launcher_core::pinyin::PinyinIndex>,
}

/// The mask of the ASCII characters that occur in `text`.
pub fn character_mask(text: &str) -> u128 {
    launcher_core::matcher::character_mask(text)
}

impl Program {
    /// Build a program and pre-compute the text used for searching.
    ///
    /// Lower-casing the haystack once at index time instead of once per query
    /// removes thousands of allocations from the hot path.
    pub fn new(
        name: String,
        full_path: PathBuf,
        icon_path: Option<PathBuf>,
        description: String,
        args: Option<String>,
        source: ProgramSourceKind,
        enabled: bool,
    ) -> Self {
        let search_text = {
            let mut s = String::with_capacity(name.len() + full_path.to_string_lossy().len() + 2);
            s.push_str(&name);
            s.push('\n');
            s.push_str(&full_path.to_string_lossy());
            s.push('\n');
            s.push_str(&description);
            s.to_lowercase()
        };
        let text_mask = character_mask(&search_text);
        let name_mask = character_mask(&name.to_lowercase());
        let pinyin = launcher_core::pinyin::PinyinIndex::build(&name);
        Self {
            name,
            full_path,
            icon_path,
            description,
            args,
            source,
            enabled,
            search_text,
            text_mask,
            name_mask,
            pinyin,
        }
    }
}

/// Options controlling what gets indexed.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Extra directories to scan.
    pub custom_directories: Vec<PathBuf>,
    /// Scan `%PATH%`.
    pub include_path_env: bool,
    /// Scan the Start Menu.
    pub include_start_menu: bool,
    /// Scan the registry `App Paths` keys.
    pub include_app_paths: bool,
    /// Scan the registry `Uninstall` keys.
    pub include_uninstall: bool,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            custom_directories: Vec::new(),
            include_path_env: true,
            include_start_menu: true,
            include_app_paths: true,
            include_uninstall: true,
        }
    }
}

/// A completed index.
#[derive(Debug, Clone, Default)]
pub struct ProgramIndex {
    /// All discovered programs, de-duplicated.
    pub programs: Vec<Program>,
    /// How long the build took.
    pub build_millis: u64,
}

impl ProgramIndex {
    /// Build the index.
    ///
    /// Every source runs on its own thread; results are merged and de-
    /// duplicated by canonical path.
    pub fn build(options: &IndexOptions) -> Self {
        let started = std::time::Instant::now();

        let mut tasks: Vec<Box<dyn Fn() -> Vec<Program> + Send + Sync>> = Vec::new();

        if options.include_start_menu {
            tasks.push(Box::new(|| {
                start_menu_directories()
                    .par_iter()
                    .flat_map_iter(|d| {
                        scan_directory(d, ProgramSourceKind::StartMenu, 0, true)
                    })
                    .collect()
            }));
        }
        if options.include_path_env {
            tasks.push(Box::new(|| {
                // `%PATH%` directories are scanned shallowly: a program "on the
                // PATH" lives directly in the directory. Recursing here would
                // pull in thousands of unrelated executables and dominate the
                // whole index build.
                let dirs = path_directories();
                dirs.par_iter()
                    .flat_map_iter(|d| {
                        scan_directory(d, ProgramSourceKind::PathEnv, 0, false)
                    })
                    .collect()
            }));
        }
        if options.include_app_paths {
            tasks.push(Box::new(app_path_programs));
        }
        if options.include_uninstall {
            tasks.push(Box::new(uninstall_programs));
        }
        if !options.custom_directories.is_empty() {
            let dirs = Arc::new(options.custom_directories.clone());
            tasks.push(Box::new(move || {
                dirs.iter()
                    .flat_map(|d| scan_directory(d, ProgramSourceKind::CustomDirectory, 0, true))
                    .collect()
            }));
        }

        let chunks: Vec<Vec<Program>> = tasks.par_iter().map(|t| t()).collect();

        // De-duplicate by lower-cased path, keeping the first sighting.
        let mut seen: HashSet<String> = HashSet::with_capacity(4096);
        let mut programs: Vec<Program> = Vec::new();
        for chunk in chunks {
            for program in chunk {
                let key = program
                    .full_path
                    .to_string_lossy()
                    .to_ascii_lowercase();
                if seen.insert(key) {
                    programs.push(program);
                }
            }
        }

        // Rank: named programs first, then shorter names, then alphabetical.
        programs.sort_by(|a, b| {
            b.enabled
                .cmp(&a.enabled)
                .then_with(|| a.name.len().cmp(&b.name.len()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Self {
            programs,
            build_millis: started.elapsed().as_millis() as u64,
        }
    }
}

/// Recursively scan a directory for launchable entries.
///
/// When `recurse` is false only the directory's own files are considered.
fn scan_directory(dir: &Path, source: ProgramSourceKind, depth: usize, recurse: bool) -> Vec<Program> {
    const MAX_DEPTH: usize = 3;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let entries: Vec<_> = entries.flatten().collect();

    // Parse shortcuts in parallel — this is where the original implementation
    // pays for COM round-trips.
    let parsed: Vec<Program> = entries
        .par_iter()
        .filter_map(|entry| program_from_path(&entry.path(), source))
        .collect();

    let mut out: Vec<Program> = parsed;

    if recurse && depth < MAX_DEPTH {
        let subdirs: Vec<PathBuf> = entries
            .iter()
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .collect();
        if !subdirs.is_empty() {
            let nested: Vec<Vec<Program>> = subdirs
                .par_iter()
                .map(|d| scan_directory(d, source, depth + 1, true))
                .collect();
            for n in nested {
                out.extend(n);
            }
        }
    }

    out
}

/// Build a [`Program`] from a filesystem entry, if it is launchable.
fn program_from_path(path: &Path, source: ProgramSourceKind) -> Option<Program> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "exe" => {
            let name = file_name_stem(path)?;
            Some(Program::new(
                name,
                path.to_path_buf(),
                Some(path.to_path_buf()),
                String::new(),
                None,
                source,
                true,
            ))
        }
        "lnk" | "url" => {
            let data = std::fs::read(path).ok()?;
            let link = lnk::parse(&data);
            let name = link
                .as_ref()
                .and_then(|l| l.name.clone())
                .filter(|n| !n.is_empty())
                .or_else(|| file_name_stem(path))?;

            let target = link
                .as_ref()
                .and_then(|l| l.target_path.clone())
                .map(PathBuf::from);

            // A shortcut that keeps its target relative writes it relative
            // to the folder the shortcut itself sits in. Resolving it here is
            // the only place where that folder is known — and a path without
            // a drive letter cannot be opened, nor can its icon be found.
            let target = target.map(|t| match path.parent() {
                Some(folder) if !t.is_absolute() => lnk::resolve_relative(folder, &t),
                _ => t,
            });

            // A target that is not there is worse than no target at all: the
            // shell still resolves the shortcut itself, and the icon of the
            // shortcut is the icon of the program it points at.
            let target = target.filter(|t| t.exists()).or_else(|| Some(path.to_path_buf()));

            Some(Program::new(
                name,
                path.to_path_buf(),
                target.clone().or_else(|| Some(path.to_path_buf())),
                link.as_ref().and_then(|l| l.description.clone()).unwrap_or_default(),
                link.as_ref().and_then(|l| l.arguments.clone()),
                source,
                true,
            ))
        }
        _ => None,
    }
}

fn file_name_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
}

/// The Start Menu program directories for the current user and all users.
fn start_menu_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(program_data) = std::env::var("ProgramData") {
        dirs.push(
            PathBuf::from(program_data)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    if let Ok(app_data) = std::env::var("APPDATA") {
        dirs.push(
            PathBuf::from(app_data)
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs"),
        );
    }
    dirs
}

/// The directories listed in `%PATH%`.
fn path_directories() -> Vec<PathBuf> {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(';')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

// ------------------------------------------------------------------ registry

#[cfg(windows)]
mod registry {
    use winreg::enums::{
        HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    };
    use winreg::RegKey;

    /// Open a subkey, trying the 64-bit and 32-bit views.
    pub fn open(root: &RegKey, path: &str) -> Option<RegKey> {
        root.open_subkey_with_flags(path, KEY_READ | KEY_WOW64_64KEY)
            .or_else(|_| root.open_subkey_with_flags(path, KEY_READ | KEY_WOW64_32KEY))
            .ok()
    }

    /// Enumerate the immediate subkeys of `root`, calling `f` for each.
    ///
    /// `winreg` 0.55 yields only subkey *names*, so each one is opened
    /// individually to obtain a handle.
    pub fn for_each_subkey(root: &RegKey, f: &mut dyn FnMut(&str, &RegKey)) {
        let names: Vec<String> = root.enum_keys().flatten().collect();
        for name in names {
            let sub = root
                .open_subkey_with_flags(&name, KEY_READ | KEY_WOW64_64KEY)
                .or_else(|_| root.open_subkey_with_flags(&name, KEY_READ | KEY_WOW64_32KEY));
            if let Ok(sub) = sub {
                f(&name, &sub);
            }
        }
    }

    /// Read a string value, returning `None` when it is missing.
    pub fn get_string(key: &RegKey, name: &str) -> Option<String> {
        key.get_value::<String, _>(name).ok()
    }

    pub fn hklm() -> RegKey {
        RegKey::predef(HKEY_LOCAL_MACHINE)
    }

    pub fn hkcu() -> RegKey {
        RegKey::predef(HKEY_CURRENT_USER)
    }
}

/// Programs registered under `App Paths`.
#[cfg(windows)]
fn app_path_programs() -> Vec<Program> {
    use rayon::prelude::*;

    const APP_PATHS: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths";

    let mut roots = vec![registry::hklm(), registry::hkcu()];
    let mut out: Vec<Program> = Vec::new();

    for root in roots.drain(..) {
        let Some(key) = registry::open(&root, APP_PATHS) else {
            continue;
        };
        let mut entries: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        registry::for_each_subkey(&key, &mut |name, sub| {
            let exe = registry::get_string(sub, "").or_else(|| {
                registry::get_string(sub, "Path").map(|p| {
                    let mut s = p;
                    if !s.ends_with('\\') {
                        s.push('\\');
                    }
                    s
                })
            });
            let desc = registry::get_string(sub, "FriendlyName");
            entries.push((name.to_string(), exe, desc));
        });

        out.extend(
            entries
                .into_par_iter()
                .filter_map(|(name, exe, desc)| {
                    let exe = exe?;
                    let path = clean_registry_path(&exe);
                    Some(Program::new(
                        name,
                        path.clone(),
                        Some(path),
                        desc.unwrap_or_default(),
                        None,
                        ProgramSourceKind::AppPaths,
                        true,
                    ))
                })
                .collect::<Vec<_>>(),
        );
    }

    out
}

/// Make a path that the registry wrote usable.
///
/// The registry keeps the path of a program in two ways that a path does not
/// understand: wrapped in quotes (`"C:\Program Files\App\app.exe"`), and
/// with the environment variables of the machine in it
/// (`%ProgramFiles%\App\app.exe`). Both are common, and both make the path
/// impossible to open — and impossible to find the icon of.
fn clean_registry_path(value: &str) -> PathBuf {
    let trimmed = value.trim().trim_matches('"').trim();
    PathBuf::from(lnk::expand_environment(trimmed))
}

/// Programs registered under `Uninstall`.
#[cfg(windows)]
fn uninstall_programs() -> Vec<Program> {
    use rayon::prelude::*;

    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";

    let mut out: Vec<Program> = Vec::new();
    for root in [registry::hklm(), registry::hkcu()] {
        let Some(key) = registry::open(&root, UNINSTALL) else {
            continue;
        };
        let mut entries: Vec<(String, Option<String>, Option<String>)> = Vec::new();
        registry::for_each_subkey(&key, &mut |name, sub| {
            let display = registry::get_string(sub, "DisplayName");
            let icon = registry::get_string(sub, "DisplayIcon");
            entries.push((name.to_string(), display, icon));
        });

        out.extend(
            entries
                .into_par_iter()
                .filter_map(|(key, display, icon)| {
                    let display = display?;
                    // DisplayIcon looks like "C:\path\app.exe,0" — strip the index.
                    let icon_path = icon.and_then(|i| {
                        let trimmed = i.trim().trim_matches('"');
                        let path = trimmed.split(',').next().unwrap_or(trimmed).trim();
                        if path.is_empty() {
                            None
                        } else {
                            Some(clean_registry_path(path))
                        }
                    });
                    Some(Program::new(
                        display,
                        icon_path.clone().unwrap_or_else(|| PathBuf::from(&key)),
                        icon_path,
                        String::new(),
                        None,
                        ProgramSourceKind::Uninstall,
                        true,
                    ))
                })
                .collect::<Vec<_>>(),
        );
    }

    out
}

#[cfg(not(windows))]
fn app_path_programs() -> Vec<Program> {
    Vec::new()
}

#[cfg(not(windows))]
fn uninstall_programs() -> Vec<Program> {
    Vec::new()
}

/// Shared, thread-safe handle to an index.
pub type SharedIndex = Arc<ProgramIndex>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_builds_without_panicking() {
        let index = ProgramIndex::build(&IndexOptions {
            custom_directories: Vec::new(),
            include_path_env: false,
            include_start_menu: false,
            include_app_paths: false,
            include_uninstall: false,
        });
        assert!(index.programs.is_empty());
    }

    #[test]
    fn custom_directory_is_scanned() {
        let dir = std::env::temp_dir().join("ideas-launcher-index-test");
        let _ = std::fs::create_dir_all(&dir);
        let exe = dir.join("HelloWorld.exe");
        std::fs::write(&exe, b"MZ").unwrap();

        let index = ProgramIndex::build(&IndexOptions {
            custom_directories: vec![dir.clone()],
            include_path_env: false,
            include_start_menu: false,
            include_app_paths: false,
            include_uninstall: false,
        });

        assert!(index.programs.iter().any(|p| p.name == "HelloWorld"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn duplicates_are_removed() {
        let dir = std::env::temp_dir().join("ideas-launcher-index-dup-test");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("A.exe"), b"MZ").unwrap();

        let index = ProgramIndex::build(&IndexOptions {
            custom_directories: vec![dir.clone(), dir.clone()],
            include_path_env: false,
            include_start_menu: false,
            include_app_paths: false,
            include_uninstall: false,
        });

        let count = index.programs.iter().filter(|p| p.name == "A").count();
        assert_eq!(count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
