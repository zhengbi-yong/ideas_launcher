//! UWP / Microsoft Store application indexing.
//!
//! Flow Launcher indexes Store apps through WinRT
//! (`Windows.Management.Deployment.PackageManager` plus
//! `Package.GetAppListEntries()`). WinRT's async model is painful to drive
//! from plain Rust, so this walks the Shell COM tree instead — the same
//! result, reached through plain `extern "system"` calls:
//!
//! 1. `SHCreateItemFromParsingName("shell:AppsFolder")` returns the virtual
//!    folder that holds every installed Store app.
//! 2. `IShellItem::BindToHandler(BHID_EnumItems)` enumerates it.
//! 3. `IShellItem2::GetDisplayName(SIGDN_NORMALDISPLAY)` gives the localised
//!    display name, and the `System.AppUserModel.ID` property gives the AUMID
//!    (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`).
//! 4. `IShellItemImageFactory::GetImage` renders the 32x32 icon.
//!
//! Launching goes through `IApplicationActivationManager::ActivateApplication`.
//!
//! Every COM call is hand written: the crate deliberately does not depend on
//! the `windows` crate, so the vtable layouts below are transcribed by hand.
//! They were checked against the installed Windows SDK
//! (`um/ShObjIdl_core.idl`), which differs from the values that circulate
//! online in a few places:
//!
//! | what                    | value                                  |
//! |-------------------------|----------------------------------------|
//! | `BHID_EnumItems`        | `{94f60519-2850-4924-aa5a-d15e84868039}` |
//! | `IID_IShellItem2`       | `{7e9fb0d3-919f-4307-ab2e-9b1860310c93}` |
//! | `IID_IEnumShellItems`   | `{70629033-e363-4a28-a567-0db78006e6d7}` |
//! | `IShellItem2::GetProperty` | vtable slot 13                    |
//! | `IShellItem2::GetString`   | vtable slot 17                    |
//! | `IApplicationActivationManager::ActivateApplication` | vtable slot 3 |
//!
//! `{00000000-0000-0000-C000-000000000046}` is *not* `BHID_EnumItems`,
//! `{b63ea76d-…}` is `IID_IShellItemArray`, and `{000214e2-…}` is
//! `IID_IShellBrowser`.
//!
//! Nothing in here ever panics. An app whose name or AUMID cannot be read is
//! skipped, an app whose icon cannot be rendered is kept without one, and the
//! enumeration as a whole degrades to an empty list rather than an error.

use std::ffi::c_void;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The edge length of the icons this module renders.
const ICON_SIZE: i32 = 32;

// ------------------------------------------------------------------- types

/// A 32-bit RGBA bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaIcon {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixels, four bytes each, in R, G, B, A order.
    pub rgba: Vec<u8>,
}

/// A UWP application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UwpApp {
    /// The localised display name, as the shell presents it.
    pub display_name: String,
    /// The description, when the shell exposes one — empty otherwise.
    pub description: String,
    /// The application user model id, e.g.
    /// `Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`.
    pub aumid: String,
    /// The 32x32 icon, when it could be rendered.
    pub icon: Option<RgbaIcon>,
}

/// The index of the UWP applications installed for the current user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UwpIndex {
    /// Every application that was found.
    pub apps: Vec<UwpApp>,
}

impl UwpIndex {
    /// Enumerate the UWP applications installed for the current user.
    ///
    /// Returns an empty index when anything goes wrong — the caller gets a
    /// usable (if empty) value instead of an error.
    pub fn enumerate() -> Self {
        #[cfg(windows)]
        {
            win::enumerate()
        }
        #[cfg(not(windows))]
        {
            Self::default()
        }
    }

    /// Launch the application with the given AUMID.
    ///
    /// Returns `true` when the shell accepted the activation request.
    pub fn activate(aumid: &str) -> bool {
        #[cfg(windows)]
        {
            win::activate(aumid)
        }
        #[cfg(not(windows))]
        {
            let _ = aumid;
            false
        }
    }

    /// Write the index to `path` as JSON.
    ///
    /// The icons are stored as PNG images (hex encoded) so that the file does
    /// not balloon into a JSON array of numbers.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let bytes = serde_json::to_vec(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Write to a temporary file first so a crash cannot corrupt the cache.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    /// Read the index back from `path`.
    ///
    /// Returns `None` when the file is missing, unreadable or malformed.
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str(&text).ok()
    }
}

// -------------------------------------------------------------------- PNG
//
// The cache stores icons as PNG. The crate has no image dependency, so a
// minimal encoder is implemented here: a PNG container around a zlib stream
// built from *stored* (uncompressed) deflate blocks. The output is a
// perfectly ordinary PNG — any decoder (including the `image` crate on the
// UI side) reads it — it just does not compress.

/// Encode RGBA pixels as a PNG image.
///
/// The pixels are written top-down, eight bits per channel, with the alpha
/// channel intact.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let stride = width as usize * 4;

    // Raw scanlines: one filter byte (0 = none) followed by the row.
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for y in 0..height as usize {
        raw.push(0);
        let start = y * stride;
        match rgba.get(start..start + stride) {
            Some(row) => raw.extend_from_slice(row),
            None => raw.extend(std::iter::repeat_n(0u8, stride)),
        }
    }

    let mut out = Vec::with_capacity(raw.len() + 128);
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(6); // colour type: truecolour with alpha
    ihdr.push(0); // compression: deflate
    ihdr.push(0); // filter method: adaptive
    ihdr.push(0); // interlace: none
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    write_chunk(&mut out, b"IEND", &[]);

    out
}

/// Decode a PNG produced by [`encode_png`] back into RGBA pixels.
///
/// Only the subset this module writes is supported: eight bit RGBA, no
/// interlacing and stored (uncompressed) deflate blocks.
fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if data.len() < MAGIC.len() || data[..MAGIC.len()] != MAGIC {
        return None;
    }

    let mut pos = MAGIC.len();
    let mut width = 0u32;
    let mut height = 0u32;
    let mut idat: Vec<u8> = Vec::new();

    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        let kind = &data[pos + 4..pos + 8];
        let start = pos + 8;
        let end = start.checked_add(len)?;
        if end + 4 > data.len() {
            return None;
        }

        match kind {
            b"IHDR" => {
                if len < 13 {
                    return None;
                }
                width = u32::from_be_bytes(data[start..start + 4].try_into().ok()?);
                height = u32::from_be_bytes(data[start + 4..start + 8].try_into().ok()?);
                let header = &data[start + 8..start + 13];
                if header[0] != 8 || header[1] != 6 || header[2] != 0 || header[3] != 0 || header[4] != 0
                {
                    return None;
                }
            }
            b"IDAT" => idat.extend_from_slice(&data[start..end]),
            b"IEND" => break,
            _ => {}
        }

        pos = end + 4;
    }

    if width == 0 || height == 0 {
        return None;
    }

    let raw = inflate_stored(&idat)?;
    let stride = width as usize * 4;
    if raw.len() < (stride + 1) * height as usize {
        return None;
    }

    let mut rgba = Vec::with_capacity(stride * height as usize);
    for y in 0..height as usize {
        let row = y * (stride + 1);
        if raw[row] != 0 {
            // Only the "none" filter is ever written by the encoder.
            return None;
        }
        rgba.extend_from_slice(&raw[row + 1..row + 1 + stride]);
    }

    Some((width, height, rgba))
}

/// Append a PNG chunk: length, type, payload and CRC.
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);

    let mut crc_data = Vec::with_capacity(4 + payload.len());
    crc_data.extend_from_slice(kind);
    crc_data.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&crc_data).to_be_bytes());
}

/// Wrap `data` in a zlib stream built from stored deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65535 * 5 + 16);

    // CM = 8 (deflate), CINFO = 7 (32 KiB window); FCHECK makes the 16 bit
    // header a multiple of 31.
    out.push(0x78);
    out.push(0x01);

    let mut chunks = data.chunks(65_535).peekable();
    loop {
        let Some(block) = chunks.next() else { break };
        let last = chunks.peek().is_none();

        out.push(u8::from(last)); // BFINAL, BTYPE = 00 (stored)
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);

        if last {
            break;
        }
    }

    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Undo [`zlib_stored`].
fn inflate_stored(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 6 {
        return None;
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 0x0f != 8 {
        return None;
    }
    if ((cmf as u16) << 8 | flg as u16) % 31 != 0 {
        return None;
    }
    if flg & 0x20 != 0 {
        // A preset dictionary is never used here.
        return None;
    }

    let mut pos = 2;
    let mut out = Vec::new();
    loop {
        if pos + 5 > data.len() {
            return None;
        }
        let header = data[pos];
        pos += 1;
        if (header >> 1) & 0x03 != 0 {
            // Only stored blocks are supported.
            return None;
        }
        let len = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        let nlen = u16::from_le_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if len != (!nlen & 0xffff) {
            return None;
        }
        pos += 4;
        if pos + len > data.len() {
            return None;
        }
        out.extend_from_slice(&data[pos..pos + len]);
        pos += len;

        if header & 0x01 != 0 {
            break;
        }
    }

    if pos + 4 <= data.len() {
        let expected = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?);
        if adler32(&out) != expected {
            return None;
        }
    }

    Some(out)
}

/// The Adler-32 checksum of `data`.
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for &byte in data {
        a = (a + byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// The CRC-32 of `data`, as PNG chunks use it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Hex encode `bytes`.
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Decode a hex string produced by [`to_hex`].
fn from_hex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)? as u8;
        let lo = (pair[1] as char).to_digit(16)? as u8;
        out.push(hi << 4 | lo);
    }
    Some(out)
}

// The icon is serialised as a PNG, not as a raw pixel array: a JSON array of
// numbers is roughly twenty times larger than the PNG of the same image.
impl Serialize for RgbaIcon {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let png = encode_png(&self.rgba, self.width, self.height);
        let mut state = serializer.serialize_struct("RgbaIcon", 3)?;
        state.serialize_field("width", &self.width)?;
        state.serialize_field("height", &self.height)?;
        state.serialize_field("png", &to_hex(&png))?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for RgbaIcon {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            width: u32,
            height: u32,
            png: String,
        }

        let raw = Raw::deserialize(deserializer)?;
        let bytes = from_hex(&raw.png)
            .ok_or_else(|| serde::de::Error::custom("icon: malformed hex"))?;
        let (width, height, rgba) =
            decode_png(&bytes).ok_or_else(|| serde::de::Error::custom("icon: malformed png"))?;
        if width != raw.width || height != raw.height {
            return Err(serde::de::Error::custom("icon: size mismatch"));
        }
        Ok(RgbaIcon {
            width,
            height,
            rgba,
        })
    }
}

// ------------------------------------------------------------------ Win32

#[cfg(windows)]
mod win {
    use super::*;

    // ------------------------------------------------------------- GUIDs

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Guid {
        pub data1: u32,
        pub data2: u16,
        pub data3: u16,
        pub data4: [u8; 8],
    }

    impl Guid {
        pub const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
            Guid {
                data1,
                data2,
                data3,
                data4,
            }
        }
    }

    /// `IID_IShellItem`
    const IID_ISHELL_ITEM: Guid =
        Guid::new(0x43826D1E, 0xE718, 0x42EE, [0xBC, 0x55, 0xA1, 0xE2, 0x61, 0xC3, 0x7B, 0xFE]);
    /// `IID_IShellItem2`
    ///
    /// Note that `{b63ea76d-…}` is `IID_IShellItemArray`, not `IShellItem2`.
    const IID_ISHELL_ITEM2: Guid =
        Guid::new(0x7E9FB0D3, 0x919F, 0x4307, [0xAB, 0x2E, 0x9B, 0x18, 0x60, 0x31, 0x0C, 0x93]);
    /// `IID_IEnumShellItems`
    ///
    /// Note that `{000214e2-…}` is `IID_IShellBrowser`.
    const IID_IENUM_SHELL_ITEMS: Guid =
        Guid::new(0x70629033, 0xE363, 0x4A28, [0xA5, 0x67, 0x0D, 0xB7, 0x80, 0x06, 0xE6, 0xD7]);
    /// `IID_IShellItemImageFactory`
    const IID_ISHELL_ITEM_IMAGE_FACTORY: Guid =
        Guid::new(0xBCC18B79, 0xBA16, 0x442F, [0x80, 0xC4, 0x8A, 0x59, 0xC3, 0x0C, 0x46, 0x3B]);
    /// `IID_IApplicationActivationManager`
    const IID_IAPPLICATION_ACTIVATION_MANAGER: Guid =
        Guid::new(0x2E941141, 0x7F97, 0x4756, [0xBA, 0x1D, 0x9D, 0xEC, 0xDE, 0x89, 0x4A, 0x3D]);
    /// `CLSID_ApplicationActivationManager`
    const CLSID_APPLICATION_ACTIVATION_MANAGER: Guid =
        Guid::new(0x45BA127D, 0x10A8, 0x46EA, [0x8A, 0xB7, 0x56, 0xEA, 0x90, 0x78, 0x94, 0x3C]);
    /// `BHID_EnumItems` — the handler that enumerates the children of a folder.
    const BHID_ENUM_ITEMS: Guid =
        Guid::new(0x94F60519, 0x2850, 0x4924, [0xAA, 0x5A, 0xD1, 0x5E, 0x84, 0x86, 0x80, 0x39]);
    /// `FMTID_SystemProperties` — the format of the `System.*` properties.
    const FMTID_SYSTEM_PROPERTIES: Guid =
        Guid::new(0xF29F85E0, 0x4FF9, 0x1068, [0xAB, 0x91, 0x08, 0x00, 0x2B, 0x27, 0xB3, 0xD9]);
    /// `FMTID_AppUserModel` — the format of the `System.AppUserModel.*` properties.
    const FMTID_APP_USER_MODEL: Guid =
        Guid::new(0x9F4C2855, 0x9F79, 0x4B39, [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3]);

    /// `PKEY_AppUserModel_ID` — the AUMID of a shell item.
    const PKEY_APP_USER_MODEL_ID: PropertyKey = PropertyKey {
        fmtid: FMTID_APP_USER_MODEL,
        pid: 5,
    };
    /// `PKEY_Comment` — `System.Comment`, the closest thing to a description.
    const PKEY_COMMENT: PropertyKey = PropertyKey {
        fmtid: FMTID_SYSTEM_PROPERTIES,
        pid: 6,
    };

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PropertyKey {
        pub fmtid: Guid,
        pub pid: u32,
    }

    // ---------------------------------------------------------- constants

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const CLSCTX_INPROC_SERVER: u32 = 1;
    /// `SIGDN_NORMALDISPLAY` — the localised display name.
    const SIGDN_NORMALDISPLAY: u32 = 0x0000_0000;
    /// `SIGDN_FILESYSPATH` — the parsing name of a shell item.
    const SIGDN_FILESYSPATH: u32 = 0x8005_8000;
    const SIIGBF_BIGGERSIZEOK: u32 = 0x0000_0001;
    const SIIGBF_ICONONLY: u32 = 0x0000_0004;
    const AO_NONE: u32 = 0;
    const BI_RGB: u32 = 0;
    const DIB_RGB_COLORS: u32 = 0;
    /// `RPC_E_CHANGED_MODE` — COM is up, but in a different apartment.
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32.cast_signed();

    // ---------------------------------------------------------- vtables

    /// A slot this module never calls.
    type UnusedFn = unsafe extern "system" fn();

    type QueryInterfaceFn =
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;
    type ReleaseFn = unsafe extern "system" fn(*mut c_void) -> u32;
    type BindToHandlerFn = unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const Guid,
        *const Guid,
        *mut *mut c_void,
    ) -> i32;
    type GetDisplayNameFn = unsafe extern "system" fn(*mut c_void, u32, *mut *mut u16) -> i32;
    type GetPropertyFn =
        unsafe extern "system" fn(*mut c_void, *const PropertyKey, *mut PropVariant) -> i32;
    type GetStringPropertyFn =
        unsafe extern "system" fn(*mut c_void, *const PropertyKey, *mut *mut u16) -> i32;
    type EnumNextFn =
        unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> i32;
    type GetImageFn = unsafe extern "system" fn(*mut c_void, Size, u32, *mut *mut c_void) -> i32;
    type ActivateApplicationFn =
        unsafe extern "system" fn(*mut c_void, *const u16, *const u16, u32, *mut u32) -> i32;

    #[repr(C)]
    struct IUnknownVtbl {
        query_interface: QueryInterfaceFn,
        #[allow(dead_code)]
        add_ref: ReleaseFn,
        release: ReleaseFn,
    }

    /// `IShellItem` — indices 0..7.
    #[repr(C)]
    #[allow(dead_code)]
    struct ShellItemVtbl {
        query_interface: QueryInterfaceFn,
        add_ref: ReleaseFn,
        release: ReleaseFn,
        bind_to_handler: BindToHandlerFn,
        get_parent: UnusedFn,
        get_display_name: GetDisplayNameFn,
        get_attributes: UnusedFn,
        compare: UnusedFn,
    }

    /// `IShellItem2` — `IShellItem` plus indices 8..20.
    ///
    /// This is the Windows 10/11 layout (`um/ShObjIdl_core.idl`): the
    /// property store comes first and `GetProperty` sits at index 13.
    #[repr(C)]
    #[allow(dead_code)]
    struct ShellItem2Vtbl {
        query_interface: QueryInterfaceFn,
        add_ref: ReleaseFn,
        release: ReleaseFn,
        bind_to_handler: BindToHandlerFn,
        get_parent: UnusedFn,
        get_display_name: GetDisplayNameFn,
        get_attributes: UnusedFn,
        compare: UnusedFn,
        // 8
        get_property_store: UnusedFn,
        // 9
        get_property_store_with_create_object: UnusedFn,
        // 10
        get_property_store_for_keys: UnusedFn,
        // 11
        get_property_description_list: UnusedFn,
        // 12
        update: UnusedFn,
        // 13
        get_property: GetPropertyFn,
        // 14
        get_class_id: UnusedFn,
        // 15
        get_file_time: UnusedFn,
        // 16
        get_int32: UnusedFn,
        // 17
        get_string: GetStringPropertyFn,
        // 18
        get_uint32: UnusedFn,
        // 19
        get_uint64: UnusedFn,
        // 20
        get_bool: UnusedFn,
    }

    /// `IEnumShellItems` — indices 0..6.
    #[repr(C)]
    #[allow(dead_code)]
    struct EnumShellItemsVtbl {
        query_interface: QueryInterfaceFn,
        add_ref: ReleaseFn,
        release: ReleaseFn,
        next: EnumNextFn,
        skip: UnusedFn,
        reset: UnusedFn,
        clone: UnusedFn,
    }

    /// `IShellItemImageFactory` — indices 0..3.
    #[repr(C)]
    #[allow(dead_code)]
    struct ShellItemImageFactoryVtbl {
        query_interface: QueryInterfaceFn,
        add_ref: ReleaseFn,
        release: ReleaseFn,
        get_image: GetImageFn,
    }

    /// `IApplicationActivationManager` — indices 0..5.
    ///
    /// `ActivateApplication` comes first on Windows 10/11.
    #[repr(C)]
    #[allow(dead_code)]
    struct ActivationManagerVtbl {
        query_interface: QueryInterfaceFn,
        add_ref: ReleaseFn,
        release: ReleaseFn,
        activate_application: ActivateApplicationFn,
        activate_for_file: UnusedFn,
        activate_for_protocol: UnusedFn,
    }

    // ----------------------------------------------------------- structs

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        cx: i32,
        cy: i32,
    }

    #[repr(C)]
    struct Bitmap {
        bm_type: i32,
        bm_width: i32,
        bm_height: i32,
        bm_width_bytes: i32,
        bm_planes: u16,
        bm_bit_count: u16,
        bm_bits: *mut c_void,
    }

    #[repr(C)]
    struct BitmapInfoHeader {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct BitmapInfo {
        bmi_header: BitmapInfoHeader,
        bmi_colors: [u32; 3],
    }

    /// A `PROPVARIANT`. Only the string variants are ever read back.
    #[repr(C)]
    struct PropVariant {
        vt: u16,
        reserved: [u16; 3],
        value: *mut c_void,
        #[cfg(target_pointer_width = "64")]
        rest: [u8; 8],
        #[cfg(target_pointer_width = "32")]
        rest: [u8; 4],
    }

    impl PropVariant {
        const VT_BSTR: u16 = 8;
        const VT_LPWSTR: u16 = 31;

        fn zeroed() -> Self {
            PropVariant {
                vt: 0,
                reserved: [0; 3],
                value: std::ptr::null_mut(),
                #[cfg(target_pointer_width = "64")]
                rest: [0; 8],
                #[cfg(target_pointer_width = "32")]
                rest: [0; 4],
            }
        }

        /// Read the variant as a string and release whatever it holds.
        fn take_string(&mut self) -> Option<String> {
            let text = match self.vt {
                Self::VT_LPWSTR | Self::VT_BSTR => {
                    let ptr = self.value as *const u16;
                    if ptr.is_null() {
                        None
                    } else {
                        Some(wide_to_string(ptr))
                    }
                }
                _ => None,
            };

            // The same thing `PropVariantClear` would do, without pulling in
            // `propsys.dll`.
            unsafe {
                match self.vt {
                    Self::VT_LPWSTR => CoTaskMemFree(self.value),
                    Self::VT_BSTR => SysFreeString(self.value as *mut u16),
                    _ => {}
                }
            }
            self.vt = 0;
            self.value = std::ptr::null_mut();
            text
        }
    }

    // ------------------------------------------------------------ imports

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(pvreserved: *mut c_void, dwcoinit: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            rclsid: *const Guid,
            punknownouter: *mut c_void,
            dwclscontext: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
        fn CoTaskMemFree(pv: *mut c_void);
        fn SysFreeString(bstr: *mut u16);
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHCreateItemFromParsingName(
            pszpath: *const u16,
            pbc: *mut c_void,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetObjectW(hobject: *mut c_void, nbytes: i32, pv: *mut c_void) -> i32;
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
        fn GetDIBits(
            hdc: *mut c_void,
            hbmp: *mut c_void,
            xstart: i32,
            ystart: u32,
            lpvbits: *mut c_void,
            lpbmi: *mut BitmapInfo,
            usage: u32,
        ) -> i32;
        fn DeleteObject(ho: *mut c_void) -> i32;
    }

    // ---------------------------------------------------------- COM glue

    /// Keeps the COM apartment alive for as long as it is held.
    struct ComGuard {
        uninitialize: bool,
    }

    impl ComGuard {
        fn new() -> Option<Self> {
            let hr = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED) };
            if hr >= 0 {
                // `S_OK` initialised COM, `S_FALSE` found it already
                // initialised in this apartment; both need a matching
                // `CoUninitialize`.
                Some(ComGuard {
                    uninitialize: true,
                })
            } else if hr == RPC_E_CHANGED_MODE {
                // COM is already up in another apartment. The shell still
                // works, we just must not tear the apartment down.
                Some(ComGuard {
                    uninitialize: false,
                })
            } else {
                None
            }
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.uninitialize {
                unsafe { CoUninitialize() }
            }
        }
    }

    /// An owning pointer to a COM object.
    struct ComPtr(*mut c_void);

    impl ComPtr {
        fn new(ptr: *mut c_void) -> Self {
            ComPtr(ptr)
        }

        fn as_ptr(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for ComPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let vtbl = vtable::<IUnknownVtbl>(self.0);
                    (vtbl.release)(self.0);
                }
                self.0 = std::ptr::null_mut();
            }
        }
    }

    /// The vtable of a COM object.
    unsafe fn vtable<T>(object: *mut c_void) -> &'static T {
        unsafe {
            let vtbl = *(object as *const *const T);
            &*vtbl
        }
    }

    /// `IUnknown::QueryInterface`.
    fn query_interface(object: *mut c_void, iid: &Guid) -> Option<ComPtr> {
        unsafe {
            let vtbl = vtable::<IUnknownVtbl>(object);
            let mut out: *mut c_void = std::ptr::null_mut();
            let hr = (vtbl.query_interface)(object, iid, &mut out);
            if hr < 0 || out.is_null() {
                None
            } else {
                Some(ComPtr::new(out))
            }
        }
    }

    /// Copy a wide string into a `String`.
    fn wide_to_string(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        unsafe {
            while *ptr.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
        }
    }

    // -------------------------------------------------------- enumeration

    /// Enumerate every UWP application.
    pub fn enumerate() -> UwpIndex {
        // The whole walk happens on a thread of its own: the apartment then
        // belongs to us and every COM object is released on the thread that
        // created it.
        let handle = std::thread::Builder::new()
            .name("uwp-index".into())
            .spawn(collect);
        let apps = match handle {
            Ok(handle) => handle.join().unwrap_or_default(),
            Err(_) => collect(),
        };

        UwpIndex { apps }
    }

    fn collect() -> Vec<UwpApp> {
        let Some(_com) = ComGuard::new() else {
            return Vec::new();
        };

        let path: Vec<u16> = "shell:AppsFolder"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut root: *mut c_void = std::ptr::null_mut();
        unsafe {
            let hr = SHCreateItemFromParsingName(
                path.as_ptr(),
                std::ptr::null_mut(),
                &IID_ISHELL_ITEM,
                &mut root,
            );
            if hr < 0 || root.is_null() {
                return Vec::new();
            }
        }
        let root = ComPtr::new(root);

        let mut enumerator: *mut c_void = std::ptr::null_mut();
        unsafe {
            let vtbl = vtable::<ShellItemVtbl>(root.as_ptr());
            let hr = (vtbl.bind_to_handler)(
                root.as_ptr(),
                std::ptr::null_mut(),
                &BHID_ENUM_ITEMS,
                &IID_IENUM_SHELL_ITEMS,
                &mut enumerator,
            );
            if hr < 0 || enumerator.is_null() {
                return Vec::new();
            }
        }
        let enumerator = ComPtr::new(enumerator);

        let mut apps: Vec<UwpApp> = Vec::new();
        loop {
            let mut item: *mut c_void = std::ptr::null_mut();
            let mut fetched: u32 = 0;
            unsafe {
                let vtbl = vtable::<EnumShellItemsVtbl>(enumerator.as_ptr());
                let hr = (vtbl.next)(enumerator.as_ptr(), 1, &mut item, &mut fetched);
                if hr < 0 || fetched == 0 || item.is_null() {
                    break;
                }
            }

            // Every failure below only skips this one application.
            let item = ComPtr::new(item);
            if let Some(app) = app_from_shell_item(item.as_ptr()) {
                apps.push(app);
            }
        }

        // The shell can expose the same app more than once.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        apps.retain(|app| seen.insert(app.aumid.clone()));

        apps.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        });

        apps
    }

    /// Build an application from one shell item.
    fn app_from_shell_item(item: *mut c_void) -> Option<UwpApp> {
        let display_name = get_display_name(item, SIGDN_NORMALDISPLAY)?;
        if display_name.trim().is_empty() {
            return None;
        }

        let aumid = {
            let item2 = query_interface(item, &IID_ISHELL_ITEM2)?;
            get_aumid(item2.as_ptr())?
        };

        let description = get_description(item);

        // A missing icon is not a reason to drop the application: it can
        // still be launched.
        let icon = get_icon(item);

        Some(UwpApp {
            display_name,
            description,
            aumid,
            icon,
        })
    }

    /// `IShellItem::GetDisplayName`.
    fn get_display_name(item: *mut c_void, kind: u32) -> Option<String> {
        unsafe {
            let vtbl = vtable::<ShellItemVtbl>(item);
            let mut raw: *mut u16 = std::ptr::null_mut();
            let hr = (vtbl.get_display_name)(item, kind, &mut raw);
            if hr < 0 || raw.is_null() {
                return None;
            }
            let text = wide_to_string(raw);
            CoTaskMemFree(raw as *mut c_void);
            Some(text)
        }
    }

    /// The AUMID of a shell item.
    ///
    /// The `System.AppUserModel.ID` property is the canonical source; the
    /// parsing name is used as a fallback.
    fn get_aumid(item2: *mut c_void) -> Option<String> {
        let mut value = get_property_string(item2, &PKEY_APP_USER_MODEL_ID);
        if value.as_deref().is_none_or(|v| !v.contains('!')) {
            // Some shell items only answer `GetString`.
            if let Some(other) = get_string_property(item2, &PKEY_APP_USER_MODEL_ID) {
                if value.is_none() || other.contains('!') {
                    value = Some(other);
                }
            }
        }

        // A Win32 application reports the path of its executable here; only
        // the Store applications use the `Family!App` form.
        if let Some(value) = value {
            if value.contains('!') {
                return Some(value);
            }
        }

        // The AUMID is also part of the parsing name.
        if let Some(path) = get_display_name(item2, SIGDN_FILESYSPATH) {
            if let Some(aumid) = aumid_from_parsing_name(&path) {
                return Some(aumid);
            }
        }
        None
    }

    /// Pull the AUMID out of a `shell:AppsFolder\...` parsing name.
    fn aumid_from_parsing_name(path: &str) -> Option<String> {
        const PREFIX: &str = "shell:appsfolder\\";
        let lower = path.to_ascii_lowercase();
        let start = lower.find(PREFIX)? + PREFIX.len();
        let aumid = path[start..].trim_matches(|c| c == '\\' || c == '/');
        if aumid.contains('!') {
            Some(aumid.to_string())
        } else {
            None
        }
    }

    /// The description of a shell item, when it has one.
    ///
    /// Store applications do not hand out the description from their
    /// manifest through the shell — only the localised display name and the
    /// AUMID — so this is usually empty.
    fn get_description(item: *mut c_void) -> String {
        let Some(item2) = query_interface(item, &IID_ISHELL_ITEM2) else {
            return String::new();
        };
        let item2 = item2.as_ptr();
        get_string_property(item2, &PKEY_COMMENT)
            .or_else(|| get_property_string(item2, &PKEY_COMMENT))
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_default()
    }

    /// `IShellItem2::GetString` (called `GetStringProperty` before
    /// Windows 8).
    fn get_string_property(item2: *mut c_void, key: &PropertyKey) -> Option<String> {
        unsafe {
            let vtbl = vtable::<ShellItem2Vtbl>(item2);
            let mut raw: *mut u16 = std::ptr::null_mut();
            let hr = (vtbl.get_string)(item2, key, &mut raw);
            if hr < 0 || raw.is_null() {
                return None;
            }
            let text = wide_to_string(raw);
            CoTaskMemFree(raw as *mut c_void);
            Some(text)
        }
    }

    /// `IShellItem2::GetProperty`, read as a string.
    fn get_property_string(item2: *mut c_void, key: &PropertyKey) -> Option<String> {
        unsafe {
            let vtbl = vtable::<ShellItem2Vtbl>(item2);
            let mut variant = PropVariant::zeroed();
            let hr = (vtbl.get_property)(item2, key, &mut variant);
            // The variant is cleared either way: a failed call may still
            // have allocated something.
            let text = variant.take_string();
            if hr < 0 {
                return None;
            }
            text
        }
    }

    // -------------------------------------------------------------- icons

    /// Render the icon of a shell item.
    fn get_icon(item: *mut c_void) -> Option<RgbaIcon> {
        let factory = query_interface(item, &IID_ISHELL_ITEM_IMAGE_FACTORY)?;
        unsafe {
            let vtbl = vtable::<ShellItemImageFactoryVtbl>(factory.as_ptr());
            let mut bitmap: *mut c_void = std::ptr::null_mut();
            let hr = (vtbl.get_image)(
                factory.as_ptr(),
                Size {
                    cx: ICON_SIZE,
                    cy: ICON_SIZE,
                },
                SIIGBF_BIGGERSIZEOK | SIIGBF_ICONONLY,
                &mut bitmap,
            );
            if hr < 0 || bitmap.is_null() {
                return None;
            }
            let image = bitmap_to_rgba(bitmap);
            DeleteObject(bitmap);
            image
        }
    }

    /// Turn an `HBITMAP` into a 32x32 RGBA image.
    fn bitmap_to_rgba(handle: *mut c_void) -> Option<RgbaIcon> {
        unsafe {
            let mut info = Bitmap {
                bm_type: 0,
                bm_width: 0,
                bm_height: 0,
                bm_width_bytes: 0,
                bm_planes: 0,
                bm_bit_count: 0,
                bm_bits: std::ptr::null_mut(),
            };
            if GetObjectW(
                handle,
                std::mem::size_of::<Bitmap>() as i32,
                &mut info as *mut Bitmap as *mut c_void,
            ) == 0
            {
                return None;
            }

            let width = info.bm_width.max(1) as usize;
            let height = info.bm_height.unsigned_abs() as usize;
            if width == 0 || height == 0 {
                return None;
            }

            let mut pixels = vec![0u8; width * height * 4];

            if !info.bm_bits.is_null() && info.bm_bit_count == 32 {
                // A DIB section: the pixels are right there, and the alpha
                // channel survives.
                let stride = if info.bm_width_bytes > 0 {
                    info.bm_width_bytes as usize
                } else {
                    width * 4
                };
                let bits = info.bm_bits as *const u8;
                for y in 0..height {
                    // The rows of a device bitmap are stored bottom-up.
                    let src_row = height - 1 - y;
                    let from = bits.add(src_row * stride);
                    let to = y * width * 4;
                    std::ptr::copy_nonoverlapping(from, pixels[to..].as_mut_ptr(), width * 4);
                }
            } else {
                // An older bitmap without a DIB section.
                let dc = GetDC(std::ptr::null_mut());
                if dc.is_null() {
                    return None;
                }
                let mut bmi = BitmapInfo {
                    bmi_header: BitmapInfoHeader {
                        bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                        bi_width: width as i32,
                        bi_height: -(height as i32),
                        bi_planes: 1,
                        bi_bit_count: 32,
                        bi_compression: BI_RGB,
                        bi_size_image: 0,
                        bi_x_pels_per_meter: 0,
                        bi_y_pels_per_meter: 0,
                        bi_clr_used: 0,
                        bi_clr_important: 0,
                    },
                    bmi_colors: [0; 3],
                };
                let lines = GetDIBits(
                    dc,
                    handle,
                    0,
                    height as u32,
                    pixels.as_mut_ptr() as *mut c_void,
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
                ReleaseDC(std::ptr::null_mut(), dc);
                if lines == 0 {
                    return None;
                }

                // `GetDIBits` does not always preserve the alpha channel.
                if !pixels.chunks_exact(4).any(|p| p[3] != 0) {
                    for pixel in pixels.chunks_exact_mut(4) {
                        pixel[3] = 255;
                    }
                }
            }

            // BGRA -> RGBA.
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }

            // Scale down to the requested size.
            let out_width = ICON_SIZE.max(1) as usize;
            let out_height = ICON_SIZE.max(1) as usize;
            let mut out = vec![0u8; out_width * out_height * 4];
            for y in 0..out_height {
                for x in 0..out_width {
                    let sx = (x as u64 * width as u64 / out_width as u64).min(width as u64 - 1) as usize;
                    let sy =
                        (y as u64 * height as u64 / out_height as u64).min(height as u64 - 1) as usize;
                    let from = (sy * width + sx) * 4;
                    let to = (y * out_width + x) * 4;
                    out[to..to + 4].copy_from_slice(&pixels[from..from + 4]);
                }
            }

            Some(RgbaIcon {
                width: out_width as u32,
                height: out_height as u32,
                rgba: out,
            })
        }
    }

    // ---------------------------------------------------------- activation

    /// Launch an application by its AUMID.
    pub fn activate(aumid: &str) -> bool {
        if aumid.trim().is_empty() {
            return false;
        }

        let Some(_com) = ComGuard::new() else {
            return false;
        };

        let mut manager: *mut c_void = std::ptr::null_mut();
        unsafe {
            let hr = CoCreateInstance(
                &CLSID_APPLICATION_ACTIVATION_MANAGER,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_IAPPLICATION_ACTIVATION_MANAGER,
                &mut manager,
            );
            if hr < 0 || manager.is_null() {
                return false;
            }
        }
        // Released before the apartment is torn down.
        let manager = ComPtr::new(manager);

        let wide: Vec<u16> = aumid.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let vtbl = vtable::<ActivationManagerVtbl>(manager.as_ptr());
            let mut process_id: u32 = 0;
            let hr = (vtbl.activate_application)(
                manager.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                AO_NONE,
                &mut process_id,
            );
            hr >= 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk the chunks of a PNG file.
    fn png_chunks(data: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let mut chunks = Vec::new();
        let mut pos = 8usize;
        while pos + 8 <= data.len() {
            let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
            let payload = data[pos + 8..pos + 8 + len].to_vec();
            chunks.push((kind, payload));
            pos += 8 + len + 4;
            if &kind == b"IEND" {
                break;
            }
        }
        chunks
    }

    #[test]
    fn png_encoder_writes_a_valid_container() {
        let pixels: Vec<u8> = vec![
            255, 0, 0, 255, // red
            0, 255, 0, 128, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 0, // transparent white
        ];

        let png = encode_png(&pixels, 2, 2);

        assert_eq!(
            &png[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            "the PNG magic number must come first"
        );

        let chunks = png_chunks(&png);
        let kinds: Vec<&[u8; 4]> = chunks.iter().map(|(k, _)| k).collect();
        assert!(kinds.contains(&b"IHDR"), "missing IHDR chunk");
        assert!(kinds.contains(&b"IDAT"), "missing IDAT chunk");
        assert!(kinds.contains(&b"IEND"), "missing IEND chunk");

        let ihdr = &chunks.iter().find(|(k, _)| k == b"IHDR").unwrap().1;
        assert_eq!(u32::from_be_bytes(ihdr[0..4].try_into().unwrap()), 2);
        assert_eq!(u32::from_be_bytes(ihdr[4..8].try_into().unwrap()), 2);
        assert_eq!(ihdr[8], 8, "bit depth");
        assert_eq!(ihdr[9], 6, "colour type RGBA");
    }

    #[test]
    fn png_encoder_round_trips_through_the_decoder() {
        let pixels: Vec<u8> = (0..16u8 * 4).collect();
        let png = encode_png(&pixels, 4, 4);
        let (width, height, decoded) = decode_png(&png).expect("the PNG must decode");
        assert_eq!(width, 4);
        assert_eq!(height, 4);
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn hex_round_trips() {
        let data = [0u8, 1, 15, 16, 127, 128, 254, 255];
        assert_eq!(from_hex(&to_hex(&data)).unwrap(), data);
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
    }

    #[test]
    fn save_and_load_round_trips() {
        let dir = std::env::temp_dir().join("ideas-launcher-uwp-cache-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("uwp-index.json");

        let index = UwpIndex {
            apps: vec![
                UwpApp {
                    display_name: "计算器".into(),
                    description: "Calculator".into(),
                    aumid: "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(),
                    icon: Some(RgbaIcon {
                        width: 2,
                        height: 2,
                        rgba: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 1, 2, 3, 4],
                    }),
                },
                UwpApp {
                    display_name: "Notepad".into(),
                    description: String::new(),
                    aumid: "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App".into(),
                    icon: None,
                },
            ],
        };

        index.save(&path).expect("the index must be written");

        let loaded = UwpIndex::load(&path).expect("the index must load");
        assert_eq!(loaded.apps.len(), 2);
        assert_eq!(loaded.apps[0].display_name, "计算器");
        assert_eq!(loaded.apps[0].aumid, index.apps[0].aumid);
        assert_eq!(loaded.apps[0].icon.as_ref().unwrap().rgba, index.apps[0].icon.as_ref().unwrap().rgba);
        assert!(loaded.apps[1].icon.is_none());

        // The icons are stored as PNG, not as a JSON array of numbers.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"png\":"), "the icon must be hex encoded PNG");
        assert!(!text.contains("\"rgba\":"), "the raw pixels must not be stored");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_of_a_missing_file_returns_none() {
        assert!(UwpIndex::load(&std::env::temp_dir().join("does-not-exist-uwp.json")).is_none());
    }

    #[test]
    fn activating_an_unknown_app_fails_without_crashing() {
        // The AUMID does not exist, so the shell refuses it — but the COM
        // plumbing must not fall over.
        assert!(!UwpIndex::activate("This.Package.DoesNotExist_8wekyb3d8bbwe!App"));
    }

    #[test]
    fn enumerates_the_installed_uwp_apps() {
        let index = UwpIndex::enumerate();

        if index.apps.is_empty() {
            eprintln!("no UWP applications were found — skipping the assertion");
            return;
        }

        eprintln!("enumerated {} UWP applications", index.apps.len());
        for app in index.apps.iter().take(15) {
            eprintln!("  {} -> {}", app.display_name, app.aumid);
        }
        if let Some(icon) = index.apps.iter().find_map(|a| a.icon.as_ref()) {
            eprintln!(
                "first icon: {}x{}, png {} bytes",
                icon.width,
                icon.height,
                encode_png(&icon.rgba, icon.width, icon.height).len()
            );
        }
        for app in index.apps.iter().take(5) {
            eprintln!(
                "  detail {} desc={:?} icon={:?}",
                app.display_name,
                app.description,
                app.icon.as_ref().map(|i| (i.width, i.height))
            );
        }
        eprintln!(
            "with an icon: {} of {}",
            index.apps.iter().filter(|a| a.icon.is_some()).count(),
            index.apps.len()
        );

        // The real index must survive a round trip through the cache.
        let dir = std::env::temp_dir().join("ideas-launcher-uwp-real-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("uwp-index.json");
        index.save(&path).expect("the index must be written");
        let loaded = UwpIndex::load(&path).expect("the index must load");
        assert_eq!(loaded.apps.len(), index.apps.len());
        assert_eq!(loaded.apps[0].display_name, index.apps[0].display_name);
        assert_eq!(loaded.apps[0].aumid, index.apps[0].aumid);
        assert_eq!(
            loaded.apps[0].icon.as_ref().map(|i| &i.rgba),
            index.apps[0].icon.as_ref().map(|i| &i.rgba),
            "the icon must survive the PNG round trip"
        );
        eprintln!(
            "cache file: {} bytes",
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!index.apps.is_empty());
        assert!(index.apps.iter().all(|a| !a.display_name.is_empty()));
        assert!(index.apps.iter().all(|a| a.aumid.contains('!')));
    }
}
