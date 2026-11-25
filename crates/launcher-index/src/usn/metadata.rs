use super::*;


/// Decode UTF-16 and push the UTF-8 form into `out`.
///
/// Surrogate pairs are joined; anything that is not a valid code point
/// becomes the replacement character, the same way
/// `String::from_utf16_lossy` would do it.

pub(in crate::usn) fn push_utf16(out: &mut String, bytes: &[u8]) {
    let count = bytes.len() / 2;
    let unit = |index: usize| u16::from_le_bytes([bytes[index * 2], bytes[index * 2 + 1]]);
    let mut index = 0usize;
    while index < count {
        let high = unit(index) as u32;
        if (0xD800..0xDC00).contains(&high) && index + 1 < count {
            let low = unit(index + 1) as u32;
            if (0xDC00..0xE000).contains(&low) {
                let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                index += 2;
                continue;
            }
        }
        out.push(char::from_u32(high).unwrap_or('\u{FFFD}'));
        index += 1;
    }
}

/// The ways the 128 bit file id can hold the file reference number.
///
/// The SDK only says `BYTE Identifier[16]`, and the NTFS driver is the only
/// authority on the layout. Rather than guess, every layout is tried and the
/// result is checked against the file index the handle reports, so a wrong
/// layout can never hand back the wrong file.
pub(in crate::usn) const FILE_ID_LAYOUTS: usize = 5;

/// Build the 128 bit identifier for one layout.
pub(in crate::usn) fn build_file_id(layout: usize, frn: u64, entry_number: u64, sequence_number: u32) -> [u8; 16] {
    let mut id = [0u8; 16];
    match layout {
        // the sequence number first, then the entry number, both little
        // endian. The sequence number is four bytes, so it sits in the first
        // four and the entry number follows it.
        0 => {
            id[0..4].copy_from_slice(&sequence_number.to_le_bytes());
            id[4..12].copy_from_slice(&entry_number.to_le_bytes());
        }
        // the whole reference number, big endian, in the upper half.
        1 => {
            id[8..].copy_from_slice(&frn.to_be_bytes());
        }
        // the whole reference number, little endian, in the upper half.
        2 => {
            id[8..].copy_from_slice(&frn.to_le_bytes());
        }
        // the whole reference number, little endian, in the lower half.
        3 => {
            id[..8].copy_from_slice(&frn.to_le_bytes());
        }
        // the whole reference number, big endian, in the lower half.
        _ => {
            id[..8].copy_from_slice(&frn.to_be_bytes());
        }
    }
    id
}

/// Read the metadata of one entry through its file reference number.
///
/// Everything opens files by id for the same purpose
/// (`OpenFileById failed %d for FRN %016I64x%016I64x`).
pub fn file_metadata(volume: &Volume, frn: u64) -> Option<FileMeta> {
    let volume_handle = unsafe {
        CreateFileW(
            wide(&volume.root).as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if volume_handle == INVALID_HANDLE_VALUE {
        return None;
    }

    // The 64 bit file reference number holds the 48 bit MFT entry number and
    // the 16 bit sequence number. That layout was checked against
    // `fsutil file queryfileid` on this machine.
    let entry_number = frn & 0x0000_FFFF_FFFF_FFFF;
    let sequence_number = ((frn >> 48) & 0xFFFF) as u32;

    let mut found = None;
    for layout in 0..FILE_ID_LAYOUTS {
        let file_id = build_file_id(layout, frn, entry_number, sequence_number);
        let Some(handle) = open_by_file_id(volume_handle, &file_id, false) else {
            continue;
        };
        // Only accept the handle when it really points at the entry that was
        // asked for.
        let index = unsafe { file_index(handle) };
        if index & 0x0000_FFFF_FFFF_FFFF != entry_number {
            unsafe { CloseHandle(handle) };
            continue;
        }
        found = unsafe { read_attributes(handle) };
        unsafe { CloseHandle(handle) };
        break;
    }
    unsafe { CloseHandle(volume_handle) };
    found
}

/// Open a file by its 128 bit identifier.
pub(in crate::usn) fn open_by_file_id(volume_handle: Handle, file_id: &[u8; 16], directory: bool) -> Option<Handle> {
    let descriptor = FileIdDescriptor {
        volume_handle,
        file_id: *file_id,
    };
    let mut object_name = UnicodeString {
        length: std::mem::size_of::<FileIdDescriptor>() as u16,
        maximum_length: std::mem::size_of::<FileIdDescriptor>() as u16,
        buffer: (&descriptor as *const FileIdDescriptor) as *mut u16,
    };
    let mut attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: std::ptr::null_mut(),
        object_name: &mut object_name,
        attributes: 0x40, // OBJ_CASE_INSENSITIVE
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let kind = if directory {
        FILE_DIRECTORY_FILE
    } else {
        FILE_NON_DIRECTORY_FILE
    };
    let mut handle: Handle = std::ptr::null_mut();
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            FILE_READ_ATTRIBUTES,
            &mut attributes,
            &mut io_status,
            std::ptr::null_mut(),
            FILE_SHARE_READ_WRITE_DELETE,
            FILE_OPEN,
            FILE_OPEN_BY_FILE_ID | kind | FILE_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || handle.is_null() {
        return None;
    }
    Some(handle)
}

/// `GetFileInformationByHandle` without pulling in an extra dependency.
#[repr(C)]
pub(in crate::usn) struct ByHandleFileInformation {
    attributes: u32,
    creation_time: [u8; 8],
    last_access_time: [u8; 8],
    last_write_time: [u8; 8],
    volume_serial_number: u32,
    file_size_high: u32,
    file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub(in crate::usn) fn GetFileInformationByHandle(
        handle: Handle,
        information: *mut ByHandleFileInformation,
    ) -> Bool;
    pub(in crate::usn) fn GetFinalPathNameByHandleW(
        handle: Handle,
        path: *mut u16,
        path_length: Dword,
        flags: Dword,
    ) -> Dword;
}

/// The full path of an open file.
unsafe fn final_path(handle: Handle) -> String {
    let mut buffer = vec![0u16; 512];
    let length = unsafe {
        GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as Dword, 0)
    };
    if length == 0 {
        return String::new();
    }
    if length as usize >= buffer.len() {
        let mut bigger = vec![0u16; length as usize + 2];
        let again = unsafe {
            GetFinalPathNameByHandleW(handle, bigger.as_mut_ptr(), bigger.len() as Dword, 0)
        };
        if again == 0 {
            return String::new();
        }
        return String::from_utf16_lossy(&bigger[..again as usize]).to_string();
    }
    String::from_utf16_lossy(&buffer[..length as usize]).to_string()
}

/// What the system says about one entry.
///
/// The path and the size are read through the file reference number, so they
/// can be held against what the reading of the master file table says.
pub fn file_identity(volume: &Volume, frn: u64) -> Option<(String, u64)> {
    let volume_handle = unsafe {
        CreateFileW(
            wide(&volume.root).as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if volume_handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let entry_number = frn & 0x0000_FFFF_FFFF_FFFF;
    let sequence_number = ((frn >> 48) & 0xFFFF) as u32;

    let mut found = None;
    for layout in 0..FILE_ID_LAYOUTS {
        let file_id = build_file_id(layout, frn, entry_number, sequence_number);
        // A folder is opened with the other flag, so both are tried.
        for directory in [false, true] {
            let Some(handle) = open_by_file_id(volume_handle, &file_id, directory) else {
                continue;
            };
            let index = unsafe { file_index(handle) };
            if index & 0x0000_FFFF_FFFF_FFFF != entry_number {
                unsafe { CloseHandle(handle) };
                continue;
            }
            let path = unsafe { final_path(handle) };
            let size = unsafe { read_attributes(handle) }
                .map(|meta| meta.size)
                .unwrap_or(0);
            unsafe { CloseHandle(handle) };
            found = Some((path, size));
            break;
        }
        if found.is_some() {
            break;
        }
    }
    unsafe { CloseHandle(volume_handle) };
    found
}

/// Read size and timestamps from an open handle.
unsafe fn read_attributes(handle: Handle) -> Option<FileMeta> {
    let mut info = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(FileMeta {
        size: ((info.file_size_high as u64) << 32) | info.file_size_low as u64,
        created: filetime_to_unix(&info.creation_time),
        modified: filetime_to_unix(&info.last_write_time),
        accessed: filetime_to_unix(&info.last_access_time),
        attributes: info.attributes,
    })
}

/// The file index an open handle points at.
///
/// For NTFS this is the file reference number, which is what the lookup is
/// checked against.
unsafe fn file_index(handle: Handle) -> u64 {
    let mut info = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    if unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) } == 0 {
        return 0;
    }
    let info = unsafe { info.assume_init() };
    ((info.file_index_high as u64) << 32) | info.file_index_low as u64
}

/// Convert a Windows FILETIME (100 ns ticks since 1601) to Unix seconds.
pub(in crate::usn) fn filetime_to_unix(raw: &[u8; 8]) -> u64 {
    let ticks = u64::from_le_bytes(*raw);
    // 11644473600 seconds between 1601-01-01 and 1970-01-01.
    ticks.saturating_sub(116_444_736_000_000_000) / 10_000_000
}

/// The attributes Everything hides by default.
pub fn is_hidden_by_default(attributes: u32) -> bool {
    attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

/// The attributes that mark a reparse point.
pub fn is_reparse_point(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// The attributes that mark a read only file.
pub fn is_read_only(attributes: u32) -> bool {
    attributes & FILE_ATTRIBUTE_READONLY != 0
}

/// Group the flat entry list into parent/child pairs.
///
/// This mirrors Everything's `convert parent frn to parent pointers` step: the
/// enumeration delivers a flat list in which every entry only knows the file
/// reference number of its folder.
pub fn build_parent_map(entries: &[UsnEntry]) -> HashMap<u64, u64> {
    let mut parents = HashMap::with_capacity(entries.len());
    for entry in entries {
        parents.insert(entry.frn, entry.parent_frn);
    }
    parents
}

/// The volume root of a path, for example `C:\` for `C:\Windows`.
pub fn volume_root(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    if text.len() < 2 {
        return None;
    }
    Some(text[..2].to_uppercase())
}