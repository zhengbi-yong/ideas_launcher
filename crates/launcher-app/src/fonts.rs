//! The fonts the launcher draws with.
//!
//! Three faces are needed, and they are put into one fallback chain so that
//! mixed text looks like one typeface:
//!
//! * **Segoe UI** for Latin letters — the face Windows itself uses.
//! * **Microsoft YaHei UI** for Chinese. `egui`'s built-in fonts contain no
//!   CJK glyphs at all, so without this every Chinese character falls back to
//!   whatever the system happens to pick, which is a serif face that does not
//!   belong next to the rest of the UI.
//! * **Segoe MDL2** for the icon glyphs.
//!
//! The order matters: the first face of the chain that owns a glyph draws
//! it. Latin therefore comes from Segoe UI and Chinese from Microsoft YaHei,
//! which is exactly how Windows draws mixed text.
//!
//! The Chinese font is the interesting one. `msyh.ttc` is 19 MB. Reading it
//! into a `Vec` would add 19 MB of anonymous memory to every launcher
//! process for the whole time it runs — a fifth of the launcher's whole
//! footprint, for a file that is never read in full. So the file is
//! *mapped* instead: the pages that are really needed are faulted in from
//! the page cache and the rest stay on disk.

use std::sync::Arc;

use crate::winapi;

/// The family name of the Latin font.
pub const LATIN_FONT_FAMILY: &str = "segoeui";

/// The family name of the Chinese font.
pub const CJK_FONT_FAMILY: &str = "msyh";

/// The family name of the icon font.
pub const ICON_FONT_FAMILY: &str = "segmdl2";

/// The Segoe UI font file.
const LATIN_FONT_PATH: &str = r"C:\Windows\Fonts\segoeui.ttf";

/// The Microsoft YaHei font collection.
const CJK_FONT_PATH: &str = r"C:\Windows\Fonts\msyh.ttc";

/// The face of the Microsoft YaHei collection to use.
///
/// The collection holds two faces: `Microsoft YaHei` and `Microsoft YaHei UI`.
/// The UI face has the tighter line metrics that small interface text needs,
/// so it is the one that is used.
const CJK_FONT_INDEX: u32 = 1;

/// Where the Segoe icon font may live.
const ICON_FONT_PATHS: &[&str] = &[
    r"C:\Windows\Fonts\segmdl2.ttf",
    r"C:\Windows\Fonts\SegoeIcons.ttf",
];

/// Install every font the launcher needs.
///
/// Missing fonts are not an error: the launcher falls back to `egui`'s own
/// fonts and stays usable, it only looks less like a Windows program.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // The chain of the proportional family, in the order the glyphs are
    // looked up.
    let mut proportional: Vec<String> = Vec::new();

    if let Some(data) = load_font(LATIN_FONT_PATH, 0) {
        fonts
            .font_data
            .insert(LATIN_FONT_FAMILY.to_owned(), data.into());
        proportional.push(LATIN_FONT_FAMILY.to_owned());
    } else {
        log::warn!("the Latin font {LATIN_FONT_PATH} could not be opened");
    }

    if let Some(data) = load_font(CJK_FONT_PATH, CJK_FONT_INDEX) {
        fonts.font_data.insert(CJK_FONT_FAMILY.to_owned(), data.into());
        proportional.push(CJK_FONT_FAMILY.to_owned());
    } else {
        log::warn!("the Chinese font {CJK_FONT_PATH} could not be opened");
    }

    // The icon font lives in a family of its own, so that its glyphs are
    // never used for ordinary text.
    for path in ICON_FONT_PATHS {
        if let Some(data) = load_font(path, 0) {
            fonts
                .font_data
                .insert(ICON_FONT_FAMILY.to_owned(), data.into());
            fonts
                .families
                .entry(egui::FontFamily::Name(ICON_FONT_FAMILY.into()))
                .or_default()
                .push(ICON_FONT_FAMILY.to_owned());
            break;
        }
    }

    // The proportional family is the one every piece of text uses. The new
    // fonts go in front of the built-in ones, so that Latin and Chinese are
    // taken from the system fonts rather than from the generic fallbacks.
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        for name in proportional.into_iter().rev() {
            family.insert(0, name);
        }
    }

    // The monospace family is used for code, and Chinese may show up there
    // too, so the Chinese face is the last fallback.
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        family.push(CJK_FONT_FAMILY.to_owned());
    }

    ctx.set_fonts(fonts);
}

/// The font id used for icon glyphs.
pub fn icon_font_id(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(ICON_FONT_FAMILY.into()))
}

/// Load a font file, or give up if it is not there.
fn load_font(path: &str, index: u32) -> Option<egui::FontData> {
    match MappedFile::open(path) {
        Some(mapped) => {
            let len = mapped.len;
            let blob: egui::epaint::text::Blob = Arc::new(mapped);
            log::info!("the font {path} was mapped ({len} bytes, face {index})");
            Some(egui::FontData::from_blob(blob, index))
        }
        None => {
            // The mapping is an optimisation, not a requirement. Reading the
            // file works everywhere, it just costs more memory.
            let bytes = std::fs::read(path).ok()?;
            log::info!("the font {path} was read ({len} bytes, face {index})", len = bytes.len());
            let mut data = egui::FontData::from_owned(bytes);
            data.index = index;
            Some(data)
        }
    }
}

/// A read-only view of a file, backed by the memory manager.
///
/// The pages are faulted in from the file as they are read, so a font that is
/// only half used only ever occupies half of its memory.
struct MappedFile {
    /// The first byte of the mapping.
    base: *const u8,
    /// The length of the file in bytes.
    len: usize,
    /// The mapping object. Has to outlive the view.
    mapping: winapi::HANDLE,
    /// The file. Has to outlive the mapping.
    file: winapi::HANDLE,
}

// The mapping is read-only and never changes while it is alive, so it may be
// shared and sent between threads like any other immutable buffer.
unsafe impl Send for MappedFile {}
unsafe impl Sync for MappedFile {}

impl MappedFile {
    /// Map a file into memory, or give up.
    fn open(path: &str) -> Option<Self> {
        use std::os::windows::ffi::OsStrExt;

        let wide: Vec<u16> = std::path::Path::new(path)
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let file = winapi::CreateFileW(
                wide.as_ptr(),
                winapi::GENERIC_READ,
                winapi::FILE_SHARE_READ,
                std::ptr::null(),
                winapi::OPEN_EXISTING,
                winapi::FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            );
            if file == winapi::INVALID_HANDLE_VALUE {
                return None;
            }

            let mut size: i64 = 0;
            if winapi::GetFileSizeEx(file, &mut size) == winapi::FALSE || size <= 0 {
                winapi::CloseHandle(file);
                return None;
            }

            let mapping = winapi::CreateFileMappingW(
                file,
                std::ptr::null(),
                winapi::PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            );
            if mapping.is_null() {
                winapi::CloseHandle(file);
                return None;
            }

            let base = winapi::MapViewOfFile(
                mapping,
                winapi::FILE_MAP_READ,
                0,
                0,
                size as usize,
            );
            if base.is_null() {
                winapi::CloseHandle(mapping);
                winapi::CloseHandle(file);
                return None;
            }

            Some(Self {
                base: base.cast(),
                len: size as usize,
                mapping,
                file,
            })
        }
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        // The mapping is read-only and lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.base, self.len) }
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        unsafe {
            winapi::UnmapViewOfFile(self.base as *mut std::ffi::c_void);
            winapi::CloseHandle(self.mapping);
            winapi::CloseHandle(self.file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapped_file_can_be_read() {
        // Every Windows has this one.
        let mapped = MappedFile::open(r"C:\Windows\Fonts\segoeui.ttf").expect("segoeui.ttf exists");
        assert!(mapped.len > 100_000, "the font should not be empty");
        let bytes: &[u8] = mapped.as_ref();
        assert_eq!(&bytes[0..4], &[0x00, 0x01, 0x00, 0x00], "a TrueType file starts with 0x00010000");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        assert!(MappedFile::open(r"C:\Windows\Fonts\there-is-no-such-font.ttf").is_none());
    }

    #[test]
    fn the_chinese_font_has_a_ui_face() {
        // The collection holds `Microsoft YaHei` and `Microsoft YaHei UI`.
        let mapped = MappedFile::open(CJK_FONT_PATH).expect("msyh.ttc exists");
        let bytes: &[u8] = mapped.as_ref();
        assert_eq!(&bytes[0..4], b"ttcf", "a font collection starts with `ttcf`");
        let faces = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        assert!(faces >= 2, "the collection should hold at least two faces");
        assert!(
            CJK_FONT_INDEX < faces,
            "the UI face must exist in the collection"
        );
    }
}
