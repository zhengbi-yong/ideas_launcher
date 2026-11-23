//! A fast, low-footprint Windows quick launcher.
//!
//! The design goal is a launcher that is *usable* in a few tens of
//! milliseconds after the hotkey is pressed. That means:
//!
//! * the window is created once and hidden, never re-created,
//! * icons are decoded lazily instead of being preloaded,
//! * the program index is built on a background thread and refreshed in
//!   place,
//! * there is no garbage collector and no JIT.

pub mod app;
pub mod details;
pub mod elevate;
pub mod fonts;
pub mod hotkey;
pub mod icons;
pub mod instance;
pub mod markdown;
pub mod mouse;
pub mod paths;
pub mod plugins;
pub mod preview;
pub mod props;
pub mod service_client;
pub mod settings_view;
pub mod shellmenu;
pub mod theme;
pub mod tray;
pub mod video;
pub mod winapi;
