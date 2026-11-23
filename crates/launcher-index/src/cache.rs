//! Persistent index cache.
//!
//! Flow Launcher keeps a MemoryPack-serialised cache
//! (`Cache\Plugins\Flow.Launcher.Plugin.Program\Win32.cache`, 504 KB in the
//! analysed install) and still spends up to 79 seconds rebuilding it.
//!
//! Here the cache is a plain JSON document that is written once after a
//! successful index build and read back at start-up, so a warm start costs a
//! single file read instead of a full filesystem sweep.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::win32::{Program, ProgramIndex, ProgramSourceKind};

/// The on-disk cache format.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    /// Format version, so an old cache can be discarded safely.
    version: u32,
    /// Unix timestamp of the build.
    built_at: u64,
    /// The programs.
    programs: Vec<CachedProgram>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedProgram {
    name: String,
    full_path: String,
    icon_path: Option<String>,
    description: String,
    args: Option<String>,
    source: String,
    enabled: bool,
}

/// Bumped to 2 when the `.lnk` parser was corrected — older caches hold the
/// broken target paths and icons.
///
/// Bumped to 3 when the target of a shortcut stopped being joined with its
/// relative path: 490 of the 2744 programs on this machine held a path
/// without a drive letter, and every one of them lost its icon.
///
/// Bumped to 4 when the paths of the registry stopped being taken as they
/// are written — wrapped in quotes, or with `%ProgramFiles%` in them.
const CACHE_VERSION: u32 = 4;

/// A cache of a built index.
#[derive(Debug, Default)]
pub struct IndexCache {
    path: PathBuf,
    /// Rebuild the index when the cache is older than this.
    pub max_age: std::time::Duration,
}

impl IndexCache {
    /// Create a cache stored at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_age: std::time::Duration::from_secs(60 * 60),
        }
    }

    /// The cache file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the cache, if it exists and is still fresh.
    pub fn load(&self) -> Option<ProgramIndex> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        let file: CacheFile = serde_json::from_str(&text).ok()?;
        if file.version != CACHE_VERSION {
            return None;
        }

        let built_at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(file.built_at);
        if built_at.elapsed().ok()? > self.max_age {
            return None;
        }

        let programs = file
            .programs
            .into_iter()
            .map(|p| {
                Program::new(
                    p.name,
                    PathBuf::from(p.full_path),
                    p.icon_path.map(PathBuf::from),
                    p.description,
                    p.args,
                    source_from_str(&p.source),
                    p.enabled,
                )
            })
            .collect();

        Some(ProgramIndex {
            programs,
            build_millis: 0,
        })
    }

    /// Write the index to the cache.
    pub fn save(&self, index: &ProgramIndex) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = CacheFile {
            version: CACHE_VERSION,
            built_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            programs: index
                .programs
                .iter()
                .map(|p| CachedProgram {
                    name: p.name.clone(),
                    full_path: p.full_path.to_string_lossy().to_string(),
                    icon_path: p
                        .icon_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_string()),
                    description: p.description.clone(),
                    args: p.args.clone(),
                    source: p.source.as_str().to_string(),
                    enabled: p.enabled,
                })
                .collect(),
        };

        // Write to a temporary file first so a crash cannot corrupt the cache.
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&file)?)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Delete the cache file.
    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn source_from_str(value: &str) -> ProgramSourceKind {
    match value {
        "start_menu" => ProgramSourceKind::StartMenu,
        "path" => ProgramSourceKind::PathEnv,
        "app_paths" => ProgramSourceKind::AppPaths,
        "uninstall" => ProgramSourceKind::Uninstall,
        _ => ProgramSourceKind::CustomDirectory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win32::IndexOptions;

    #[test]
    fn round_trips_an_index() {
        let dir = std::env::temp_dir().join("ideas-launcher-cache-test");
        let path = dir.join("program-index.json");
        let cache = IndexCache::new(&path);

        let index = ProgramIndex::build(&IndexOptions {
            custom_directories: Vec::new(),
            include_path_env: false,
            include_start_menu: false,
            include_app_paths: false,
            include_uninstall: false,
        });
        cache.save(&index).unwrap();

        let loaded = cache.load().expect("cache should load");
        assert_eq!(loaded.programs.len(), index.programs.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_cache_returns_none() {
        let cache = IndexCache::new(std::env::temp_dir().join("does-not-exist.json"));
        assert!(cache.load().is_none());
    }
}
