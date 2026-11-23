//! The properties the shell knows about a file.
//!
//! The file manager shows more about a file than the file system knows: how
//! long a video runs, which camera took a picture, what kind of document it
//! is. All of that comes from the **property system of the shell**, and the
//! shell is asked for it by name — `System.Media.Duration` and the like — so
//! that no one has to remember the numbers behind the names.
//!
//! The names are turned into the keys of the properties by the system itself
//! (`PSGetPropertyKeyFromName`), and a name the system does not know is
//! simply left out.
//!
//! The COM declarations are the ones of `launcher-index`'s `uwp` module, which
//! walks the same shell interfaces for the Store applications.

use std::ffi::c_void;
use std::path::Path;

use parking_lot::Mutex;

/// A GUID, as the shell hands them out.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl Guid {
    const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
        Self {
            data1,
            data2,
            data3,
            data4,
        }
    }
}

/// `IID_IShellItem2` — a file the shell knows, with its properties.
const IID_ISHELL_ITEM2: Guid = Guid::new(
    0x7e9fb0d3,
    0x919f,
    0x4307,
    [0xab, 0x2e, 0x9b, 0x18, 0x60, 0x31, 0x0c, 0x93],
);

/// The key of a property: the number that tells them apart and the GUID of
/// the set they belong to.
#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyKey {
    format_id: Guid,
    property_id: u32,
}

/// `PROPVARIANT` — the way the shell hands a value over.
///
/// The structure is **24 bytes** on a 64 bit machine: the kind and three
/// words of room, and then a union of 16 bytes. Writing a smaller one and
/// handing it over makes the system read past the end of it.
#[repr(C)]
#[derive(Clone, Copy)]
struct PropVariant {
    kind: u16,
    reserved: [u16; 3],
    /// The value: a number, or a pointer to a string.
    value: u64,
    rest: [u8; 8],
}

/// The kinds of value that are read.
const VT_I4: u16 = 3;
const VT_UI4: u16 = 19;
const VT_I8: u16 = 20;
const VT_UI8: u16 = 21;
const VT_BSTR: u16 = 8;
const VT_LPWSTR: u16 = 31;

#[link(name = "shell32")]
unsafe extern "system" {
    fn SHCreateItemFromParsingName(
        path: *const u16,
        folder: *mut c_void,
        iid: *const Guid,
        item: *mut *mut c_void,
    ) -> i32;
}

#[link(name = "propsys")]
unsafe extern "system" {
    /// The key of a property, from its name.
    fn PSGetPropertyKeyFromName(name: *const u16, key: *mut PropertyKey) -> i32;
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(memory: *mut c_void);
    fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
    fn CoUninitialize();
}

#[link(name = "oleaut32")]
unsafe extern "system" {
    fn SysFreeString(bstr: *mut c_void);
}

/// `COINIT_MULTITHREADED` — the thread is not to have a message pump.
const COINIT_MULTITHREADED: u32 = 0;

/// The COM apartment of this thread.
///
/// The shell is asked about a file with COM, and COM has to be told that the
/// thread is here — otherwise it answers `CO_E_NOTINITIALIZED` and knows
/// nothing.
///
/// The answer of `CoInitializeEx` is looked at. `S_OK` means it worked and
/// `S_FALSE` means COM was already there — both have to be undone when the
/// thread ends. `RPC_E_CHANGED_MODE` means somebody else made this thread an
/// apartment of another kind, and then there is nothing to undo: the calls
/// still work, but in that apartment.
struct ComApartment {
    /// Whether COM was started here and has to be stopped again.
    owned: std::cell::Cell<bool>,
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owned.get() {
            unsafe { CoUninitialize() };
        }
    }
}

thread_local! {
    /// The apartment of this thread.
    static COM: ComApartment = ComApartment { owned: std::cell::Cell::new(false) };
}

/// Make sure that COM is running on this thread.
pub fn initialize_com() {
    COM.with(|cell| {
        if cell.owned.get() {
            return;
        }
        let code = unsafe { CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED) };
        // Zero is `S_OK`, one is `S_FALSE`, and everything else is a failure.
        cell.owned.set(code >= 0);
        if code < 0 {
            log::warn!("COM could not be started on this thread: {code:#x}");
        }
    });
}

/// The methods of `IShellItem2`, in the order of the interface.
///
/// The first eight are the ones of `IShellItem` — the shell puts
/// `BindToHandler` in the fourth place — and the properties come after
/// that.
#[repr(C)]
struct ShellItem2Vtbl {
    _query_interface: usize,
    _add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    _middle: [usize; 10],
    get_property: unsafe extern "system" fn(*mut c_void, *const PropertyKey, *mut PropVariant) -> i32,
    _three: [usize; 3],
    get_string: unsafe extern "system" fn(*mut c_void, *const PropertyKey, *mut *mut u16) -> i32,
    _tail: [usize; 8],
}

/// The table of functions of a COM object.
unsafe fn vtable<T>(object: *mut c_void) -> *const T {
    unsafe { *((object as *const *const T)) }
}

/// Release a COM object.
unsafe fn release(object: *mut c_void) {
    unsafe {
        let vt = vtable::<ShellItem2Vtbl>(object);
        ((*vt).release)(object);
    }
}

/// The properties of a file, by name.
pub struct Properties {
    /// The values that the shell knows, in the order they were asked for.
    pub values: Vec<(String, Option<String>)>,
}

/// Read the named properties of a file.
///
/// The names that the system does not know are left out, and so are the
/// properties that the file does not have.
pub fn read(path: &Path, names: &[&str]) -> Properties {
    initialize_com();
    let mut values = Vec::new();
    let Some(item) = shell_item(path) else {
        return Properties { values };
    };

    for name in names {
        let value = item_value(item, name);
        values.push((name.to_string(), value));
    }
    unsafe {
        release(item);
    }
    Properties { values }
}

/// The file as the shell knows it.
fn shell_item(path: &Path) -> Option<*mut c_void> {
    let wide = wide_of(path);
    unsafe {
        let mut item: *mut c_void = std::ptr::null_mut();
        let ok = SHCreateItemFromParsingName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &IID_ISHELL_ITEM2,
            &mut item,
        );
        if ok < 0 || item.is_null() {
            None
        } else {
            Some(item)
        }
    }
}

/// The value of one property, in words.
fn item_value(item: *mut c_void, name: &str) -> Option<String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = PropertyKey {
        format_id: Guid::new(0, 0, 0, [0; 8]),
        property_id: 0,
    };
    unsafe {
        if PSGetPropertyKeyFromName(wide.as_ptr(), &mut key) < 0 {
            return None;
        }
        let vt = vtable::<ShellItem2Vtbl>(item);
        let mut value = PropVariant {
            kind: 0,
            reserved: [0; 3],
            value: 0,
            rest: [0; 8],
        };
        let ok = ((*vt).get_property)(item, &key, &mut value);
        if ok < 0 {
            return None;
        }
        let text = match value.kind {
            VT_UI4 | VT_I4 => Some((value.value as u32 as i64).to_string()),
            VT_UI8 | VT_I8 => Some(value.value.to_string()),
            VT_LPWSTR => {
                let pointer = value.value as *const u16;
                if pointer.is_null() {
                    None
                } else {
                    let mut length = 0;
                    while *pointer.add(length) != 0 {
                        length += 1;
                    }
                    let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
                    CoTaskMemFree(pointer as *mut c_void);
                    Some(text)
                }
            }
            VT_BSTR => {
                // A BSTR is **not** ended with a zero: the number of
                // characters is in the four bytes in front of it. Reading it
                // as if it were a zero-terminated string walks past the end
                // of the buffer, and freeing it with `CoTaskMemFree` breaks
                // the heap — a BSTR is freed with `SysFreeString`.
                let pointer = value.value as *mut u16;
                if pointer.is_null() {
                    None
                } else {
                    let length = *((pointer as *const u8).sub(4) as *const u32) as usize;
                    let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
                    SysFreeString(pointer as *mut c_void);
                    Some(text)
                }
            }
            _ => None,
        };
        text.filter(|text| !text.is_empty())
    }
}

/// The path as a wide string, with the terminator.
fn wide_of(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// The names of the properties of a video.
pub const VIDEO_PROPERTIES: &[&str] = &[
    "System.Media.Duration",
    "System.Video.FrameWidth",
    "System.Video.FrameHeight",
    "System.Video.FrameRate",
    "System.Video.EncodingBitrate",
    "System.Video.Compression",
    "System.Media.DateEncoded",
];

/// The names of the properties of a picture.
pub const IMAGE_PROPERTIES: &[&str] = &[
    "System.Photo.CameraModel",
    "System.Photo.DateTaken",
    "System.Photo.ExposureTime",
    "System.Photo.FNumber",
    "System.Photo.ISOSpeed",
    "System.Photo.Orientation",
];

/// The names of the properties of any file.
pub const FILE_PROPERTIES: &[&str] = &[
    "System.Comment",
    "System.Author",
    "System.Title",
    "System.Subject",
    "System.Category",
];

/// The names of the properties of a document.
pub const DOCUMENT_PROPERTIES: &[&str] = &[
    "System.Document.PageCount",
    "System.Document.WordCount",
    "System.Document.CharacterCount",
    "System.Document.PageWidth",
    "System.Document.PageHeight",
];

/// Keep the compiler from complaining about the unused import when the
/// module is compiled without the parking lot.
#[allow(dead_code)]
fn _unused(_: &Mutex<()>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_is_not_there_has_no_properties() {
        let properties = read(Path::new(r"C:\definitely\not\here.mp4"), VIDEO_PROPERTIES);
        assert!(properties.values.iter().all(|(_, value)| value.is_none()));
    }

    #[test]
    fn the_names_of_the_properties_are_known() {
        initialize_com();
        // The keys of the properties are asked of the system by name. These
        // are the ones the details of a file are made of, and the system has
        // to know them — a name it does not know is simply left out, but
        // these are the ones that matter.
        for name in [
            "System.Media.Duration",
            "System.Video.FrameWidth",
            "System.Video.FrameHeight",
            "System.Video.FrameRate",
            "System.Video.Compression",
            "System.Photo.CameraModel",
            "System.Photo.DateTaken",
            "System.Comment",
            "System.Author",
        ] {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let mut key = PropertyKey {
                format_id: Guid::new(0, 0, 0, [0; 8]),
                property_id: 0,
            };
            let ok = unsafe { PSGetPropertyKeyFromName(wide.as_ptr(), &mut key) };
            assert!(ok >= 0, "the system does not know {name} ({ok:#x})");
        }
    }
}
