//! Windows program indexing.
//!
//! Re-implements the six index sources observed in Flow Launcher's
//! `Flow.Launcher.Plugin.Program.dll` (`Win32` class):
//!
//! 1. `StartMenuPrograms`   — Start Menu folders
//! 2. `PATHPrograms`        — every directory on `%PATH%`
//! 3. `AppPathsPrograms`    — the registry `App Paths` keys
//! 4. `UnregisteredPrograms`— the registry `Uninstall` keys
//! 5. `EnumerateProgramsInDir` — user configured directories
//! 6. `GetProgramPathFromRegistrySubKeys` — vendor specific registry subkeys
//!
//! Unlike the original, the sources are walked in parallel and shortcuts are
//! resolved by parsing the `.lnk` file directly (see [`lnk`]) instead of
//! going through `IShellLink` over COM.

pub mod cache;
pub mod files;
pub mod fsindex;
pub mod lnk;
pub mod mft;
pub mod uwp;
pub mod usn;
pub mod watch;
pub mod win32;

pub use cache::IndexCache;
pub use files::{FileIndex, ScanProgress, SearchOptions};
pub use fsindex::{FsHit, FsIndex};
pub use uwp::{RgbaIcon, UwpApp, UwpIndex};
pub use win32::{IndexOptions, Program, ProgramIndex, ProgramSourceKind};
