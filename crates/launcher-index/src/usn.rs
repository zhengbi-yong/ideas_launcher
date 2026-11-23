//! NTFS indexing through the USN / MFT enumeration interfaces.
//!
//! Everything (voidtools) indexes NTFS volumes by reading the Master File Table
//! instead of walking the directory tree: the whole table is read in one
//! sequential pass, which is what makes an instant full-disk index possible.
//! The signed 1.4.1 release carries the debug strings of that code path, so
//! the technique is documented rather than guessed:
//!
//! * `.\src\ntfs.c` — the source file the strings come from,
//! * `ntfs_enum_mft` — the enumeration entry point,
//! * `bad NTFS signature: %t` / `mft_lcn %I64u` — the boot sector is read to
//!   locate the MFT,
//! * `FSCTL_GET_NTFS_FILE_RECORD failed %u` — individual MFT records are read
//!   through that control code,
//! * `convert parent frn to parent pointers: %f seconds` — the flat list of
//!   (name, parent FRN) pairs is turned into a folder tree afterwards.
//!
//! This module uses the same idea but through `FSCTL_ENUM_USN_DATA`, the
//! supported control code that hands out a `USN_RECORD` for every MFT entry
//! in one call. It yields the same complete name list without having to
//! parse the on-disk MFT records, and it keeps the code small enough to
//! maintain.
//!
//! The USN *change journal* is used by Everything for the opposite job: it
//! keeps the index up to date while the program runs
//! (`_db_monitor_ntfs_process_usn_records`). That part is not implemented
//! here yet; see the report for the plan.

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;

type Handle = *mut c_void;
type Bool = i32;
type Dword = u32;
type NtStatus = i32;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;

const GENERIC_READ: Dword = 0x8000_0000;
const FILE_SHARE_READ: Dword = 0x0000_0001;
const FILE_SHARE_WRITE: Dword = 0x0000_0002;
const OPEN_EXISTING: Dword = 3;

const DRIVE_FIXED: u32 = 3;

/// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 44, METHOD_BUFFERED, FILE_ANY_ACCESS)`
const FSCTL_ENUM_USN_DATA: Dword = 0x0009_00B3;
/// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 40, METHOD_BUFFERED, FILE_ANY_ACCESS)`
const FSCTL_QUERY_USN_JOURNAL: Dword = 0x0009_00F4;
const FSCTL_READ_USN_JOURNAL: Dword = 0x0009_00BB;

/// The file attributes the index needs.
pub const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
pub const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
pub const FILE_ATTRIBUTE_SYSTEM: u32 = 0x0000_0004;
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// The reason bits that mark a record as no longer live.
const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
const USN_REASON_FILE_DELETE: u32 = 0x0000_2000;

const FILE_OPEN_BY_FILE_ID: Dword = 0x0000_2000;
/// Opening a folder needs this flag.
const FILE_DIRECTORY_FILE: Dword = 0x0000_0001;
const FILE_READ_ATTRIBUTES: Dword = 0x0080;
/// The right to change the attributes of a file.
const FILE_WRITE_ATTRIBUTES: Dword = 0x0100;
/// The right to wait for the file.
const SYNCHRONIZE: Dword = 0x0010_0000;
const FILE_SHARE_READ_WRITE_DELETE: Dword = 0x0000_0007;
const FILE_OPEN: Dword = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: Dword = 0x0000_0040;
const FILE_OPEN_REPARSE_POINT: Dword = 0x0020_0000;
/// Opening a directory needs this flag.
const FILE_FLAG_BACKUP_SEMANTICS: Dword = 0x0200_0000;

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit_handle: Bool,
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: Handle,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: NtStatus,
    information: usize,
}

/// The locally unique identifier of a privilege.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Luid {
    low_part: u32,
    high_part: u32,
}

/// A privilege together with the attributes it should get.
#[repr(C)]
struct LuidAndAttributes {
    luid: Luid,
    attributes: u32,
}

/// The list of privileges that `AdjustTokenPrivileges` changes.
#[repr(C)]
struct TokenPrivileges {
    privilege_count: u32,
    control: u32,
    privileges: [LuidAndAttributes; 1],
}

#[repr(C)]
struct FileIdDescriptor {
    volume_handle: Handle,
    file_id: [u8; 16],
}

/// `MFT_ENUM_DATA_V0`, the input of `FSCTL_ENUM_USN_DATA`.
#[repr(C)]
struct MftEnumDataV0 {
    start_file_reference_number: u64,
    low_usn: i64,
    high_usn: i64,
}

/// `USN_JOURNAL_DATA_V0`, the output of `FSCTL_READ_USN_JOURNAL`.
#[repr(C)]
pub struct UsnJournalDataV0 {
    pub usn_journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    pub maximum_size: u64,
    pub allocation_delta: u64,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: Dword,
        share_mode: Dword,
        security_attributes: *mut SecurityAttributes,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template_file: Handle,
    ) -> Handle;
    fn CloseHandle(object: Handle) -> Bool;
    fn GetCurrentProcess() -> Handle;
    fn OpenProcessToken(process: Handle, desired_access: Dword, token: *mut Handle) -> Bool;
    fn DeviceIoControl(
        device: Handle,
        control_code: Dword,
        in_buffer: *const c_void,
        in_buffer_size: Dword,
        out_buffer: *mut c_void,
        out_buffer_size: Dword,
        bytes_returned: *mut Dword,
        overlapped: *mut c_void,
    ) -> Bool;
    fn GetLastError() -> Dword;
    fn SetLastError(code: Dword);
    fn GetDriveTypeW(root_path: *const u16) -> u32;
    fn GetLogicalDrives() -> Dword;
    fn GetVolumeInformationW(
        root_path: *const u16,
        volume_name_buffer: *mut u16,
        volume_name_size: Dword,
        volume_serial_number: *mut Dword,
        maximum_component_length: *mut Dword,
        file_system_flags: *mut Dword,
        file_system_name_buffer: *mut u16,
        file_system_name_size: Dword,
    ) -> Bool;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtOpenFile(
        file_handle: *mut Handle,
        desired_access: Dword,
        object_attributes: *mut ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *mut i64,
        share_access: Dword,
        create_disposition: Dword,
        create_options: Dword,
        ea_buffer: *mut c_void,
        ea_length: Dword,
    ) -> NtStatus;
    /// Turn a privilege on by its number.
    ///
    /// This is the helper the older tools use; it takes the privilege number
    /// rather than its name and needs no buffer of its own.
    fn RtlAdjustPrivilege(
        privilege: u32,
        enable: Bool,
        new_state: *mut TokenPrivileges,
        old_state: *mut TokenPrivileges,
    ) -> NtStatus;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetTokenInformation(
        token: Handle,
        token_information_class: i32,
        token_information: *mut c_void,
        token_information_length: Dword,
        return_length: *mut Dword,
    ) -> Bool;
    fn LookupPrivilegeValueW(
        system_name: *const u16,
        name: *const u16,
        luid: *mut Luid,
    ) -> Bool;
    fn LookupPrivilegeNameW(
        system_name: *const u16,
        luid: *mut Luid,
        name: *mut u16,
        size: *mut Dword,
    ) -> Bool;
    fn AdjustTokenPrivileges(
        token: Handle,
        disable_all_privileges: Bool,
        new_state: *mut TokenPrivileges,
        buffer_length: Dword,
        previous_state: *mut c_void,
        return_length: *mut Dword,
    ) -> Bool;
}

/// The fields of one record of the master file table.
///
/// The name of the record is not part of it: it is handed over separately,
/// because it has to be turned into text.
pub use crate::mft::RecordFields;

/// The metadata Everything reads out of the MFT for one entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileMeta {
    /// Size in bytes.
    pub size: u64,
    /// Creation time, seconds since the Unix epoch.
    pub created: u64,
    /// Modification time, seconds since the Unix epoch.
    pub modified: u64,
    /// Access time, seconds since the Unix epoch.
    pub accessed: u64,
    /// The Win32 file attributes.
    pub attributes: u32,
}

/// One entry read out of the MFT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsnEntry {
    /// The 64-bit NTFS file reference number.
    pub frn: u64,
    /// The file reference number of the containing folder.
    pub parent_frn: u64,
    /// The file or folder name.
    pub name: String,
    /// The Win32 file attributes.
    pub attributes: u32,
}

impl UsnEntry {
    /// True for folders.
    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// One entry of the MFT, with the name living in a shared arena.
///
/// The arena form saves one allocation per file: a volume with a million
/// files would otherwise allocate and free a million strings before the
/// index even starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEntry {
    /// The 64-bit NTFS file reference number.
    pub frn: u64,
    /// The file reference number of the containing folder.
    pub parent_frn: u64,
    /// The offset of the name inside the arena.
    pub name_start: u32,
    /// The length of the name in bytes.
    pub name_len: u32,
    /// The Win32 file attributes.
    pub attributes: u32,
    /// Why the record changed, when the source says so.
    ///
    /// The change journal carries it (`USN_REASON_FILE_CREATE` and its
    /// friends), and it is what tells a new file from a deleted one.
    /// The master file table does not always carry it.
    pub reason: u32,
    /// The number of the change that last touched the file.
    ///
    /// It is what says **when** the file changed — and it is how the files
    /// that changed since a given moment are picked out.
    pub usn: u64,
    /// The size of the content, when it is known.
    pub size: u64,
    /// When the file was created, in seconds since the Unix epoch.
    pub created: u32,
    /// When the content changed the last time.
    pub modified: u32,
    /// When the file was read the last time.
    pub accessed: u32,
    /// Whether the metadata above is known.
    ///
    /// The change journal does not carry the size or the times, so the entries
    /// that come from it have to look the file up.
    pub meta_known: bool,
}

impl RawEntry {
    /// True for folders.
    pub fn is_dir(&self) -> bool {
        self.attributes & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// Read the master file table straight off the disk.
///
/// This is the fast path. The table is read once, in order, and every entry
/// comes with its size and its three timestamps — so the filters on the size
/// and on the times are exact right away.
///
/// Everything does it this way (`ntfs_enum_mft`, `mft_lcn %I64u`), and the
/// difference shows: on this machine the change journal takes 383 seconds
/// for the same table.
/// Read the master file table straight off the disk and hand every entry
/// over as it is read.
///
/// This is the fast path. The table is read once, in order, and every entry
/// comes with its size and its three timestamps — so the filters on the size
/// and on the times are exact right away.
///
/// Everything does it this way (`ntfs_enum_mft`, `mft_lcn %I64u`), and the
/// difference shows: on this machine the change journal takes 383 seconds
/// for the same table.
pub fn enumerate_mft_with(
    volume: &Volume,
    progress: Option<&(dyn Fn(usize) + Sync)>,
    visit: impl FnMut(&str, crate::mft::RecordFields),
) -> Result<crate::mft::MftStats, crate::mft::MftError> {
    enumerate_mft_with_bytes(volume, progress, None, visit)
}

/// Read the master file table, saying how many bytes have been read.
///
/// The second report is called with the number of bytes that have been read
/// and the number of bytes the table has altogether, so that the caller can
/// show a bar that fills up.
pub fn enumerate_mft_with_bytes(
    volume: &Volume,
    progress: Option<&(dyn Fn(usize) + Sync)>,
    bytes: Option<&(dyn Fn(u64, u64) + Sync)>,
    mut visit: impl FnMut(&str, crate::mft::RecordFields),
) -> Result<crate::mft::MftStats, crate::mft::MftError> {
    let reader = crate::mft::MftReader::open(volume)?;
    let mut count = 0usize;
    let stats = reader.for_each_record_with_progress(
        |name, fields| {
            visit(name, fields);
            count += 1;
            if let Some(report) = progress {
                report(count);
            }
        },
        bytes,
    );
    Ok(stats)
}

/// Read every entry of the master file table of one volume into a list.
///
/// This is the slow way round: the entries are collected first and turned
/// into the index afterwards. [`enumerate_mft_with`] is the one the launcher
/// uses, because it does not need the list.
pub fn enumerate_mft(
    volume: &Volume,
    names: &mut String,
    progress: Option<&(dyn Fn(usize) + Sync)>,
) -> Result<Vec<RawEntry>, crate::mft::MftError> {
    let mut entries = Vec::new();
    enumerate_mft_with(volume, progress, |name, fields| {
        // The root of the volume has an empty name; it is the anchor of the
        // tree rather than an entry of its own.
        if name.is_empty() {
            return;
        }
        let name_start = names.len() as u32;
        names.push_str(name);
        entries.push(RawEntry {
            frn: (fields.sequence as u64) << 48 | (fields.number & 0x0000_FFFF_FFFF_FFFF),
            parent_frn: fields.parent,
            name_start,
            name_len: name.len() as u32,
            attributes: if fields.is_directory {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                0
            },
            // The master file table does not say why the record changed.
            reason: 0,
            usn: 0,
            size: fields.size,
            created: fields.created as u32,
            modified: fields.modified as u32,
            accessed: fields.accessed as u32,
            meta_known: true,
        });
    })?;
    Ok(entries)
}

/// Turn a list of records into the arena form.
///
/// Used by the tests and by anything that already has the records at hand.
pub fn collect_raw(records: &[UsnEntry]) -> (String, Vec<RawEntry>) {
    let mut names = String::new();
    let mut raw = Vec::with_capacity(records.len());
    for record in records {
        let name_start = names.len() as u32;
        names.push_str(&record.name);
        raw.push(RawEntry {
            frn: record.frn,
            parent_frn: record.parent_frn,
            name_start,
            name_len: record.name.len() as u32,
            attributes: record.attributes,
            // The entries of the test helper do not say why they changed.
            reason: 0,
            usn: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
            meta_known: false,
        });
    }
    (names, raw)
}

/// An NTFS volume that can be indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// The volume root, for example `C:\`.
    pub root: String,
    /// The device path, for example `\\.\C:`.
    pub device: String,
    /// The volume label, when it has one.
    pub label: String,
}

/// Why a volume could not be indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsnError {
    /// The volume could not be opened.
    OpenFailed(u32),
    /// The control code was rejected.
    IoFailed(u32),
    /// The process is not elevated and the volume needs it.
    NeedsAdministrator(u32),
    /// The data that came back could not be understood.
    Malformed,
}

impl std::fmt::Display for UsnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsnError::OpenFailed(code) => write!(f, "opening the volume failed ({code})"),
            UsnError::IoFailed(code) => write!(f, "the control code was rejected ({code})"),
            UsnError::NeedsAdministrator(code) => write!(
                f,
                "indexing NTFS volumes needs administrative privileges (error {code})"
            ),
            UsnError::Malformed => write!(f, "the volume returned malformed data"),
        }
    }
}

impl std::error::Error for UsnError {}

/// Turn a Rust string into a NUL terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Every fixed NTFS volume on this machine.
///
/// The check is the same one Everything makes: read the file system name and
/// keep the volumes that report `NTFS`.
pub fn ntfs_volumes() -> Vec<Volume> {
    let mut out = Vec::new();
    // The number of every volume that has been seen, so that a drive that
    // points at a volume that is already there is only counted once.
    let mut seen: Vec<Dword> = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = format!("{letter}:\\");
        let root_wide = wide(&root);
        // Only fixed drives: network shares and removable media are indexed
        // by the folder walk instead.
        if unsafe { GetDriveTypeW(root_wide.as_ptr()) } != DRIVE_FIXED {
            continue;
        }
        let mut fs_name = [0u16; 64];
        let mut label = [0u16; 256];
        let mut serial: Dword = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                root_wide.as_ptr(),
                label.as_mut_ptr(),
                label.len() as Dword,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as Dword,
            )
        };
        if ok == 0 {
            continue;
        }
        let fs = String::from_utf16_lossy(&fs_name[..fs_name.iter().position(|c| *c == 0).unwrap_or(0)]);
        if !fs.eq_ignore_ascii_case("NTFS") {
            continue;
        }
        // A drive can point at a volume that is already there: `subst X: C:\Some`
        // makes X: a fixed drive with the file system of the disk it points at.
        // Such a drive would be scanned a second time, and the volume
        // signature would change as soon as it appears — which throws the
        // saved table away, and building a new one needs administrative
        // rights. The number of the volume tells the two apart, so the
        // drive that comes first in the alphabet keeps it.
        if serial != 0 {
            if seen.contains(&serial) {
                continue;
            }
            seen.push(serial);
        }
        let label_text = String::from_utf16_lossy(
            &label[..label.iter().position(|c| *c == 0).unwrap_or(0)],
        );
        out.push(Volume {
            device: format!("\\\\.\\{letter}:"),
            root,
            label: label_text,
        });
    }
    out
}

/// Read the state of the USN change journal of a volume.
///
/// The control code is a file system one, so it needs a handle on the volume
/// root directory rather than on the volume device.
pub fn journal_state(volume: &Volume) -> Option<UsnJournalDataV0> {
    // The journal is only readable with the volume privilege.
    let _ = enable_manage_volume_privilege();
    let handle = unsafe {
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
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut out = UsnJournalDataV0 {
        usn_journal_id: 0,
        first_usn: 0,
        next_usn: 0,
        lowest_valid_usn: 0,
        max_usn: 0,
        maximum_size: 0,
        allocation_delta: 0,
    };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            // The state of the journal is asked for with the query control
            // code, which takes no input; the read control code is for the
            // records themselves.
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            (&mut out as *mut UsnJournalDataV0).cast(),
            std::mem::size_of::<UsnJournalDataV0>() as Dword,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return None;
    }
    Some(out)
}

/// The state of the change journal of the volume with that root.
///
/// The volume is only known by its root here — the change journal does not
/// need anything else.
pub fn journal_state_at(root: &str) -> Option<UsnJournalDataV0> {
    journal_state(&Volume {
        root: root.to_string(),
        device: String::new(),
        label: String::new(),
    })
}

/// The control codes this module uses, with the names from `winioctl.h`.
///
/// The values were taken from the Windows SDK header
/// (`um\winioctl.h`) and verified against the running system:
///
/// ```text
/// FSCTL_ENUM_USN_DATA         CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 44, METHOD_NEITHER, FILE_ANY_ACCESS)
/// FSCTL_READ_USN_JOURNAL      CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 46, METHOD_NEITHER, FILE_ANY_ACCESS)
/// FSCTL_QUERY_USN_JOURNAL     CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 61, METHOD_BUFFERED, FILE_ANY_ACCESS)
/// FSCTL_GET_NTFS_VOLUME_DATA  CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 25, METHOD_BUFFERED, FILE_ANY_ACCESS)
/// FSCTL_GET_NTFS_FILE_RECORD  CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 26, METHOD_BUFFERED, FILE_ANY_ACCESS)
/// ```
pub const FSCTL_CODES: &[(&str, Dword)] = &[
    ("FSCTL_ENUM_USN_DATA", 0x0009_00B3),
    ("FSCTL_READ_USN_JOURNAL", 0x0009_00BB),
    ("FSCTL_QUERY_USN_JOURNAL", 0x0009_00F4),
    ("FSCTL_GET_NTFS_VOLUME_DATA", 0x0009_0064),
    ("FSCTL_GET_NTFS_FILE_RECORD", 0x0009_0068),
];

/// Call a set of control codes and report what the system answered.
///
/// Used to tell "the control code is wrong" apart from "the process is not
/// elevated": the Windows error codes say which one it is
/// (`ERROR_ACCESS_DENIED` is 5, `ERROR_INVALID_PARAMETER` is 87,
/// `ERROR_INVALID_FUNCTION` is 1 and `ERROR_NOT_SUPPORTED` is 50).
pub fn probe_control_codes(volume: &Volume) -> Vec<String> {
    const FILE_READ_DATA: Dword = 0x0000_0001;
    const FILE_FLAG_BACKUP_SEMANTICS: Dword = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: Dword = 0x0020_0000;
    let mut out = Vec::new();

    // (handle path, access, flags)
    let device = volume.device.clone();
    let root = volume.root.clone();
    let shapes: &[(&str, Dword, Dword)] = &[
        (&device, FILE_READ_ATTRIBUTES, 0),
        (&root, FILE_READ_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS),
        (
            &root,
            FILE_READ_ATTRIBUTES,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        ),
        (&root, FILE_READ_DATA, FILE_FLAG_BACKUP_SEMANTICS),
        // The volume device itself, read in full. This is what `fsutil` does.
        (&device, GENERIC_READ, 0),
        (&device, GENERIC_READ, FILE_FLAG_BACKUP_SEMANTICS),
        (&device, FILE_READ_DATA, 0),
    ];

    for (path, access, flags) in shapes {
        let handle = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                *access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                *flags,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            out.push(format!(
                "  open {path} access={access:#x} flags={flags:#x} failed: {}",
                unsafe { GetLastError() }
            ));
            continue;
        }
        out.push(format!(
            "  open {path} access={access:#x} flags={flags:#x} ok"
        ));
        for (name, code) in FSCTL_CODES {
            let request = MftEnumDataV0 {
                start_file_reference_number: 0,
                low_usn: 0,
                high_usn: -1,
            };
            let mut buffer = vec![0u8; 64 * 1024];
            let mut returned = 0u32;
            let ok = unsafe {
                DeviceIoControl(
                    handle,
                    *code,
                    (&request as *const MftEnumDataV0).cast(),
                    std::mem::size_of::<MftEnumDataV0>() as Dword,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as Dword,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            let error = unsafe { GetLastError() };
            out.push(format!(
                "    {name:<32} ({code:#x}) -> ok={ok} error={error} bytes={returned}"
            ));
        }
        unsafe { CloseHandle(handle) };
    }
    out
}

/// The reason bits of the change journal.
pub mod reason {
    /// The file was created.
    pub const FILE_CREATE: u32 = 0x0000_0100;
    /// The file was deleted.
    pub const FILE_DELETE: u32 = 0x0000_0200;
    /// The file got its old name.
    pub const RENAME_OLD_NAME: u32 = 0x0000_1000;
    /// The file got its new name.
    pub const RENAME_NEW_NAME: u32 = 0x0000_2000;
}

/// What happened to a file, according to the change journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalChange {
    /// The 64-bit file reference number.
    pub frn: u64,
    /// The file reference number of the containing folder.
    pub parent_frn: u64,
    /// The name of the file.
    pub name: String,
    /// The reason bits of the change.
    pub reason: u32,
    /// The Win32 file attributes.
    pub attributes: u32,
}

/// The input of `FSCTL_READ_USN_JOURNAL`.
///
/// The name is worth spelling out, because the layout with `StartUsn`,
/// `HighUsn`, `LowUsn` and the two version numbers is **`MFT_ENUM_DATA_V1`** —
/// the input of `FSCTL_ENUM_USN_DATA`, which enumerates the master file table.
/// Handing that one to the driver that reads the change journal is what made
/// every call fail with `ERROR_INVALID_USER_BUFFER` (1784) — it was tried in
/// a thousand ways, and every one of them used the wrong shape.
///
/// The layout Windows actually wants (from `um\winioctl.h`):
///
/// ```text
/// offset  size  field
///      0     8  StartUsn            ← 完整的 64 位变更号
///      8     4  ReasonMask
///     12     4  ReturnOnlyOnClose
///     16     8  Timeout
///     24     8  BytesToWaitFor
///     32     8  UsnJournalID
///     40     2  MinMajorVersion
///     42     2  MaxMajorVersion
/// ```
///
/// That is 44 bytes of fields, and **48 bytes of structure** once the padding
/// to the eight bytes of a `DWORDLONG` is counted.
#[repr(C)]
struct ReadUsnJournalDataV1 {
    start_usn: u64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
    min_major_version: u16,
    max_major_version: u16,
}

/// Read the changes of the change journal.
///
/// Everything does this to keep its index alive
/// (`_db_monitor_ntfs_process_usn_records`). The records that come back say
/// what happened to a file, so the index can be brought up to date without
/// reading the table again.
///
/// Returns the changes and the number of the journal after them, or the
/// error code of the system when the journal could not be read.
///
/// **The error code is worth keeping.** On this machine the read is turned
/// down with `ERROR_INVALID_USER_BUFFER` (1784) — the volume is there, the
/// privilege is there, the journal is full of records (Microsoft's own
/// `fsutil usn readjournal` reads 315,000 of them) — and without the number
/// in the log there is nothing to go on.
pub fn read_journal(
    volume: &Volume,
    after_usn: i64,
) -> Result<(Vec<JournalChange>, i64), u32> {
    let (raw, _) = read_journal_raw(volume, after_usn, 1024 * 1024)?;
    let mut changes = Vec::new();
    let mut last_usn = after_usn;
    let mut offset = 0usize;
    while offset + USN_RECORD_V2_HEADER <= raw.len() {
        let Some((record, record_length)) = parse_usn_record(&raw, offset) else {
            // There is no record at this place. The buffer does not always
            // start with one — the driver can hand out the tail of the record
            // that came before the one that was asked for — so the search
            // moves on by one byte and tries again.
            offset += 1;
            continue;
        };
        // The place to start at next time is the number of the **last**
        // record that was read — and it never goes backwards, so that a
        // driver that hands out records from the beginning of the journal
        // cannot make the thread read the same records for ever.
        last_usn = last_usn.max(record.usn);
        if record.usn > after_usn {
            let name = decode_utf16(record.name);
            changes.push(JournalChange {
                frn: record.frn,
                parent_frn: record.parent_frn,
                name,
                reason: record.reason,
                attributes: record.attributes,
            });
        }
        offset += record_length;
    }
    Ok((changes, last_usn))
}

/// Read the change journal and hand back the bytes as the driver wrote them.
///
/// The parsing lives in [`read_journal`]; this one stops at the raw bytes so
/// that a wrong guess about the layout can be looked at instead of guessed at.
pub fn read_journal_raw(
    volume: &Volume,
    after_usn: i64,
    buffer_size: usize,
) -> Result<(Vec<u8>, u32), u32> {
    let _ = enable_manage_volume_privilege();
    let handle = unsafe {
        CreateFileW(
            wide(&volume.root).as_ptr(),
            // The handle wants to **read** the volume. The old code asked for
            // three odd rights instead, and the driver turned the request
            // down; `GENERIC_READ` is what the documentation for the control
            // code asks for, and what `fsutil` asks for.
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(unsafe { GetLastError() });
    }
    // The number of the journal, so that the records of another one are not
    // read.
    let journal_id = journal_state(volume)
        .map(|state| state.usn_journal_id)
        .unwrap_or(0);
    let request = ReadUsnJournalDataV1 {
        start_usn: after_usn as u64,
        // Every reason is wanted.
        reason_mask: 0xFFFF_FFFF,
        return_only_on_close: 0,
        // Come back right away, and do not wait for more.
        timeout: 0,
        bytes_to_wait_for: 0,
        usn_journal_id: journal_id,
        min_major_version: 2,
        max_major_version: 3,
    };
    let mut buffer = vec![0u8; buffer_size];
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_READ_USN_JOURNAL,
            (&request as *const ReadUsnJournalDataV1).cast(),
            std::mem::size_of::<ReadUsnJournalDataV1>() as Dword,
            buffer.as_mut_ptr().cast(),
            buffer.len() as Dword,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Err(unsafe { GetLastError() });
    }
    buffer.truncate(returned as usize);
    Ok((buffer, returned))
}

/// The number of records in a raw buffer.
pub fn count_raw_records(raw: &[u8]) -> usize {
    let mut count = 0usize;
    let mut offset = 0usize;
    while offset + USN_RECORD_V2_HEADER <= raw.len() {
        let Some((_record, length)) = parse_usn_record(raw, offset) else {
            offset += 1;
            continue;
        };
        count += 1;
        offset += length;
    }
    count
}

/// The records of a raw buffer whose name holds the given text.
///
/// This is how a change of the journal is looked at: the reason says what
/// happened, and the reason is what the index acts on.
pub fn describe_records_matching(raw: &[u8], text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + USN_RECORD_V2_HEADER <= raw.len() && out.len() < max {
        let Some((record, length)) = parse_usn_record(raw, offset) else {
            offset += 1;
            continue;
        };
        let name = decode_utf16(record.name);
        if name.contains(text) {
            out.push(format!(
                "at {offset}: usn {}, frn 0x{:x}, parent 0x{:x}, reason 0x{:x}, attributes 0x{:x}, name '{}'",
                record.usn, record.frn, record.parent_frn, record.reason, record.attributes, name
            ));
        }
        offset += length;
    }
    out
}

/// Describe the records in a raw buffer, without believing them.
///
/// Every header field is printed as it stands, so that a buffer that does not
/// look the way it should shows *where* it differs. The walk stops at the
/// first header that cannot be believed, and says so.
pub fn describe_raw_records(raw: &[u8], max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset + USN_RECORD_V2_HEADER <= raw.len() && out.len() < max {
        let Some((record, record_length)) = parse_usn_record(raw, offset) else {
            out.push(format!("at {offset}: no record here"));
            offset += 1;
            continue;
        };
        let name = decode_utf16(record.name);
        out.push(format!(
            "at {offset}: length {record_length}, usn {}, frn {}, parent {}, reason 0x{:x}, \
             attributes 0x{:x}, name '{}'",
            record.usn,
            record.frn,
            record.parent_frn,
            record.reason,
            record.attributes,
            name
        ));
        offset += record_length;
    }
    out
}

/// Decode UTF-16 bytes into a string.
fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// Read every entry of the MFT of one volume.
///
/// This is the equivalent of Everything's `ntfs_enum_mft`: one sequential
/// pass over the whole table, in the order the entries live in the table.
///
/// The names are written into `names`, which becomes the arena of the index.
/// The progress callback is told how many entries have been read so far; it
/// is called after every round, so it should be cheap.
pub fn enumerate_volume(
    volume: &Volume,
    names: &mut String,
    progress: Option<&(dyn Fn(usize) + Sync)>,
) -> Result<Vec<RawEntry>, UsnError> {
    let mut entries = Vec::new();
    enumerate_volume_with(volume, progress, |name, mut entry| {
        let name_start = names.len() as u32;
        names.push_str(name);
        entry.name_start = name_start;
        entries.push(entry);
    })?;
    Ok(entries)
}

/// Read every entry of the change journal of one volume, handing the entries
/// over as they are read.
///
/// This is the slow path: the change journal is read through the driver,
/// which walks the whole table over and over again for every slice of it.
/// The launcher only uses it when the table cannot be read directly.
pub fn enumerate_volume_with(
    volume: &Volume,
    progress: Option<&(dyn Fn(usize) + Sync)>,
    visit: impl FnMut(&str, RawEntry),
) -> Result<usize, UsnError> {
    // Reading the master file table needs the "manage the volume" privilege.
    // An administrator has it, but it is off by default, so it is switched
    // on here.
    let privilege = enable_manage_volume_privilege();
    // The control code belongs to the file system, so the handle is opened on
    // the volume itself. `fsutil usn enumData` does the same and gets the
    // whole table out of it.
    //
    // Reading the master file table needs `FILE_READ_DATA`, which in turn
    // needs administrative privileges; Everything asks for the same in its
    // own words:
    // `"Everything" requires administrative privileges to index NTFS volumes.`
    let mut handle = unsafe {
        CreateFileW(
            wide(&volume.device).as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        // Some setups only allow the volume root directory to be opened.
        handle = unsafe {
            CreateFileW(
                wide(&volume.root).as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
    }
    if handle == INVALID_HANDLE_VALUE {
        let code = unsafe { GetLastError() };
        return Err(if code == 5 {
            UsnError::NeedsAdministrator(code)
        } else {
            UsnError::OpenFailed(code)
        });
    }
    // The upper bound of the change numbers has to cover every record in the
    // table, otherwise the driver walks over the whole table again and again
    // to find the few records that fall inside the range. The next number of
    // the change journal is exactly that bound.
    let journal_high = journal_state(volume)
        .map(|state| state.next_usn)
        .filter(|next| *next > 0);
    let result = enumerate_with_handle(handle, journal_high, progress, visit);
    unsafe { CloseHandle(handle) };
    let _ = privilege;
    result
}

/// True when the process runs with an elevated token.
///
/// Reading the master file table needs it. Everything asks for the same in
/// its own words:
/// `"Everything" requires administrative privileges to index NTFS volumes.`
pub fn is_elevated() -> bool {
    const TOKEN_QUERY: Dword = 0x0008;
    const TOKEN_ELEVATION_CLASS: i32 = 20;

    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: u32,
    }

    let mut token: Handle = std::ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return false;
    }
    let mut elevation = TokenElevation { token_is_elevated: 0 };
    let mut returned = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            (&mut elevation as *mut TokenElevation).cast(),
            std::mem::size_of::<TokenElevation>() as Dword,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.token_is_elevated != 0
}

/// The raw privilege buffer of the process token.
///
/// Used to find out how the privileges are laid out and which identifier the
/// volume privilege carries.
pub fn token_privilege_bytes() -> Vec<u8> {
    const TOKEN_QUERY: Dword = 0x0008;
    const TOKEN_PRIVILEGES_CLASS: i32 = 3;

    let mut token: Handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Vec::new();
    }
    let size = 4096u32;
    let mut buffer = vec![0u8; size as usize];
    let mut returned = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_PRIVILEGES_CLASS,
            buffer.as_mut_ptr().cast(),
            size,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Vec::new();
    }
    buffer.truncate(returned as usize);
    buffer
}

/// The privileges the process token holds, with their attributes.
///
/// Used to find out why a privilege could not be turned on.
pub fn token_privileges() -> Vec<(String, u32)> {
    const TOKEN_QUERY: Dword = 0x0008;
    const TOKEN_PRIVILEGES_CLASS: i32 = 3;

    let mut token: Handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut size = 1024u32;
    for _ in 0..8 {
        let mut buffer = vec![0u8; size as usize];
        let mut returned = 0u32;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TOKEN_PRIVILEGES_CLASS,
                buffer.as_mut_ptr().cast(),
                size,
                &mut returned,
            )
        };
        if ok == 0 {
            size *= 2;
            continue;
        }
        let count = u32::from_le_bytes(buffer[0..4].try_into().unwrap()) as usize;
        // Each entry is a LUID (8 bytes) followed by the attributes.
        for index in 0..count {
            let base = 8 + index * 12;
            if base + 12 > buffer.len() {
                break;
            }
            let low = u32::from_le_bytes(buffer[base..base + 4].try_into().unwrap());
            let high = u32::from_le_bytes(buffer[base + 4..base + 8].try_into().unwrap());
            let attributes = u32::from_le_bytes(buffer[base + 8..base + 12].try_into().unwrap());
            let mut name_buffer = [0u16; 256];
            let mut name_length = name_buffer.len() as Dword;
            let ok = unsafe {
                LookupPrivilegeNameW(
                    std::ptr::null(),
                    &mut Luid {
                        low_part: low,
                        high_part: high,
                    },
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                )
            };
            let name = if ok == 0 {
                format!("luid:{low:08x}:{high:08x}")
            } else {
                let end = name_buffer
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(name_buffer.len());
                String::from_utf16_lossy(&name_buffer[..end])
            };
            out.push((name, attributes));
        }
        break;
    }
    unsafe { CloseHandle(token) };
    out
}

/// What happened while the privilege was being turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivilegeOutcome {
    /// True when the privilege is on.
    pub enabled: bool,
    /// The step that failed, when it did.
    pub stage: &'static str,
    /// The error code of that step.
    pub error: u32,
    /// The identifier of the privilege, for comparing with the token.
    pub luid: (u32, u32),
}

/// The number of the "manage the volume" privilege.
///
/// The numbers are the ones the system uses; `SeManageVolumePrivilege` is
/// number 15.
pub const PRIVILEGE_MANAGE_VOLUME: u32 = 15;

/// Turn on a privilege by name and report what happened.
///
/// `(enabled, stage, error)`.
///
/// Several ways of doing this are tried, because the exact shape of the
/// privilege list is one of those things the headers do not spell out and
/// that has to be found by asking the system. Whichever way works wins, and
/// the others are reported as failures.
pub fn enable_privilege(name: &str) -> (bool, &'static str, u32) {
    const TOKEN_ADJUST_PRIVILEGES: Dword = 0x0020;
    const TOKEN_QUERY: Dword = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;

    let mut token: Handle = std::ptr::null_mut();
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return (false, "OpenProcessToken", unsafe { GetLastError() });
    }
    let wide_name = wide(name);
    let mut luid = Luid {
        low_part: 0,
        high_part: 0,
    };
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), wide_name.as_ptr(), &mut luid) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        return (false, "LookupPrivilegeValueW", error);
    }

    let mut last_error = 0u32;
    for layout in 0..3 {
        // 0: count, control, entry — the shape from the header.
        // 1: count, entry — the shape the system hands back.
        // 2: count, control, padding, entry.
        let mut buffer = [0u8; 24];
        let entry = match layout {
            0 => 8usize,
            1 => 4usize,
            _ => 12usize,
        };
        buffer[0..4].copy_from_slice(&1u32.to_le_bytes());
        buffer[entry..entry + 4].copy_from_slice(&luid.low_part.to_le_bytes());
        buffer[entry + 4..entry + 8].copy_from_slice(&luid.high_part.to_le_bytes());
        buffer[entry + 8..entry + 12].copy_from_slice(&SE_PRIVILEGE_ENABLED.to_le_bytes());

        let mut returned = 0u32;
        unsafe { SetLastError(0) };
        let ok = unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                buffer.as_mut_ptr().cast(),
                (entry + 12) as Dword,
                std::ptr::null_mut(),
                &mut returned,
            )
        };
        let error = unsafe { GetLastError() };
        if ok != 0 && error == 0 {
            unsafe { CloseHandle(token) };
            return (true, layout_name(layout), 0);
        }
        last_error = error;
    }
    unsafe { CloseHandle(token) };

    // The other way round: the helper from ntdll, which knows the layout by
    // itself and only needs the number of the privilege.
    let number = privilege_number(name);
    if number != 0 {
        let status = unsafe {
            RtlAdjustPrivilege(number, 1, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if status == 0 {
            return (true, "RtlAdjustPrivilege", 0);
        }
        last_error = status as u32;
    }
    (false, "AdjustTokenPrivileges", last_error)
}

/// The name of a privilege list layout.
fn layout_name(layout: usize) -> &'static str {
    match layout {
        0 => "AdjustTokenPrivileges/header",
        1 => "AdjustTokenPrivileges/count-only",
        _ => "AdjustTokenPrivileges/padded",
    }
}

/// The number of a privilege, for [`RtlAdjustPrivilege`].
fn privilege_number(name: &str) -> u32 {
    match name {
        "SeCreateTokenPrivilege" => 2,
        "SeIncreaseQuotaPrivilege" => 3,
        "SeIncreaseBasePriorityPrivilege" => 4,
        "SeAssignPrimaryTokenPrivilege" => 5,
        "SeTcbPrivilege" => 6,
        "SeSecurityPrivilege" => 7,
        "SeTakeOwnershipPrivilege" => 8,
        "SeLoadDriverPrivilege" => 9,
        "SeSystemProfilePrivilege" => 10,
        "SeSystemtimePrivilege" => 11,
        "SeChangeNotifyPrivilege" => 12,
        "SeRemoteShutdownPrivilege" => 13,
        "SeUndockPrivilege" => 14,
        "SeManageVolumePrivilege" => PRIVILEGE_MANAGE_VOLUME,
        "SeCreateGlobalPrivilege" => 16,
        "SeDebugPrivilege" => 20,
        "SeImpersonatePrivilege" => 21,
        "SeCreateSymbolicLinkPrivilege" => 22,
        _ => 0,
    }
}

/// Turn on the "Manage the volume" privilege.
///
/// This is the piece that is easy to miss. An administrator account *has*
/// the privilege, but it is disabled by default and has to be switched on
/// explicitly with `AdjustTokenPrivileges` — which is exactly what `fsutil`
/// does before it reads a change journal. Without it, `FSCTL_ENUM_USN_DATA`
/// answers `ERROR_ACCESS_DENIED` even in an elevated process.
pub fn enable_manage_volume_privilege() -> bool {
    enable_manage_volume_privilege_detailed().enabled
}

/// Turn on the privilege and report where it went wrong.
pub fn enable_manage_volume_privilege_detailed() -> PrivilegeOutcome {
    const TOKEN_ADJUST_PRIVILEGES: Dword = 0x0020;
    const TOKEN_QUERY: Dword = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;

    let mut token: Handle = std::ptr::null_mut();
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return PrivilegeOutcome {
            enabled: false,
            stage: "OpenProcessToken",
            error: unsafe { GetLastError() },
            luid: (0, 0),
        };
    }

    let name = wide("SeManageVolumePrivilege");
    let mut luid = Luid {
        low_part: 0,
        high_part: 0,
    };
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        return PrivilegeOutcome {
            enabled: false,
            stage: "LookupPrivilegeValueW",
            error,
            luid: (luid.low_part, luid.high_part),
        };
    }

    let mut last_error = 0u32;
    for layout in 0..3 {
        // 0: count, control, entry — the shape from the header.
        // 1: count, entry — the shape the system hands back.
        // 2: count, control, padding, entry.
        let mut buffer = [0u8; 24];
        let entry = match layout {
            0 => 8usize,
            1 => 4usize,
            _ => 12usize,
        };
        buffer[0..4].copy_from_slice(&1u32.to_le_bytes());
        buffer[entry..entry + 4].copy_from_slice(&luid.low_part.to_le_bytes());
        buffer[entry + 4..entry + 8].copy_from_slice(&luid.high_part.to_le_bytes());
        buffer[entry + 8..entry + 12].copy_from_slice(&SE_PRIVILEGE_ENABLED.to_le_bytes());

        let mut returned = 0u32;
        unsafe { SetLastError(0) };
        let ok = unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                buffer.as_mut_ptr().cast(),
                (entry + 12) as Dword,
                std::ptr::null_mut(),
                &mut returned,
            )
        };
        let error = unsafe { GetLastError() };
        if ok != 0 && error == 0 {
            unsafe { CloseHandle(token) };
            return PrivilegeOutcome {
                enabled: true,
                stage: layout_name(layout),
                error: 0,
                luid: (luid.low_part, luid.high_part),
            };
        }
        last_error = error;
    }
    unsafe { CloseHandle(token) };

    // The other way round: the helper from ntdll, which knows the layout by
    // itself and only needs the number of the privilege.
    let status = unsafe {
        RtlAdjustPrivilege(
            PRIVILEGE_MANAGE_VOLUME,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if status == 0 {
        return PrivilegeOutcome {
            enabled: true,
            stage: "RtlAdjustPrivilege",
            error: 0,
            luid: (luid.low_part, luid.high_part),
        };
    }
    PrivilegeOutcome {
        enabled: false,
        stage: "AdjustTokenPrivileges",
        error: last_error,
        luid: (luid.low_part, luid.high_part),
    }
}

/// One parsed record of the change journal.
///
/// The journal holds two shapes of record: the older one with a 64-bit file
/// number (`USN_RECORD_V2`) and the newer one with a 128-bit number
/// (`USN_RECORD_V3`). Windows writes the newer one as soon as the volume has
/// 128-bit file numbers turned on, and that is the default now, so both have
/// to be understood. The 128-bit number holds the ordinary 64-bit number in
/// its low half, and that is the number the rest of the program works with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawRecord<'a> {
    frn: u64,
    parent_frn: u64,
    /// The number of the change that produced this record.
    usn: i64,
    reason: u32,
    attributes: u32,
    /// The name, still in UTF-16.
    name: &'a [u8],
}

/// The size of the header of a `USN_RECORD_V2`.
const USN_RECORD_V2_HEADER: usize = 60;
/// The size of the header of a `USN_RECORD_V3`.
const USN_RECORD_V3_HEADER: usize = 76;

/// Parse one record of the change journal out of the buffer.
///
/// The two layouts, taken from the Windows headers:
///
/// ```text
/// offset  size  field                     V2      V3
///      0     4  RecordLength               same    same
///      4     2  MajorVersion               same    same
///      6     2  MinorVersion               same    same
///      8     8  FileReferenceNumber        same    16 bytes
///     16     8  ParentFileReferenceNumber  same    16 bytes
///     24     8  Usn                        same    +16
///     32     8  TimeStamp                  same    +16
///     40     4  Reason                     same    +16
///     44     4  SourceInfo                 same    +16
///     48     4  SecurityId                 same    +16
///     52     4  FileAttributes             same    +16
///     56     2  FileNameLength             same    +16
///     58     2  FileNameOffset             same    +16
///     60   ...  FileName                   same    +16
/// ```
///
/// Returns the record and its length, or `None` when there is no record at
/// that place. Version 4 records belong to ReFS and carry 128 bit references
/// in a different shape; they are not read.
fn parse_usn_record(
    buffer: &[u8],
    offset: usize,
) -> Option<(RawRecord<'_>, usize)> {
    if offset + USN_RECORD_V2_HEADER > buffer.len() {
        return None;
    }
    let record_length =
        u32::from_le_bytes(buffer[offset..offset + 4].try_into().unwrap()) as usize;
    if record_length < USN_RECORD_V2_HEADER || offset + record_length > buffer.len() {
        return None;
    }
    let major_version = u16::from_le_bytes(buffer[offset + 4..offset + 6].try_into().unwrap());
    // The two layouts differ only in the size of the two file numbers, so
    // everything after them moves by sixteen bytes.
    let (frn, parent_frn, usn, reason, attributes, header_size, name_length_at, name_offset_at) =
        match major_version {
            2 => (
                u64::from_le_bytes(buffer[offset + 8..offset + 16].try_into().unwrap()),
                u64::from_le_bytes(buffer[offset + 16..offset + 24].try_into().unwrap()),
                i64::from_le_bytes(buffer[offset + 24..offset + 32].try_into().unwrap()),
                u32::from_le_bytes(buffer[offset + 40..offset + 44].try_into().unwrap()),
                u32::from_le_bytes(buffer[offset + 52..offset + 56].try_into().unwrap()),
                USN_RECORD_V2_HEADER,
                56usize,
                58usize,
            ),
            3 => (
                // The 128 bit number holds the ordinary number in its low half.
                u64::from_le_bytes(buffer[offset + 8..offset + 16].try_into().unwrap()),
                u64::from_le_bytes(buffer[offset + 24..offset + 32].try_into().unwrap()),
                i64::from_le_bytes(buffer[offset + 40..offset + 48].try_into().unwrap()),
                u32::from_le_bytes(buffer[offset + 56..offset + 60].try_into().unwrap()),
                u32::from_le_bytes(buffer[offset + 68..offset + 72].try_into().unwrap()),
                USN_RECORD_V3_HEADER,
                72usize,
                74usize,
            ),
            // Version 4 belongs to ReFS.
            _ => return None,
        };
    let name_length =
        u16::from_le_bytes(buffer[offset + name_length_at..offset + name_length_at + 2].try_into().unwrap())
            as usize;
    let name_offset =
        u16::from_le_bytes(buffer[offset + name_offset_at..offset + name_offset_at + 2].try_into().unwrap())
            as usize;

    let name_start = offset + name_offset;
    let name_end = name_start + name_length;
    // The name cannot start inside the header, and it has to end inside the
    // record. Both together keep a wrong header from being taken for a real
    // one.
    if name_offset < header_size || name_end > offset + record_length || name_length % 2 != 0 {
        return None;
    }

    Some((
        RawRecord {
            frn,
            parent_frn,
            usn,
            reason,
            attributes,
            name: &buffer[name_start..name_end],
        },
        record_length,
    ))
}

/// Try the enumeration with a few parameter shapes and report what came back.
///
/// The driver is picky about the range of change numbers and about the size
/// of the input structure, and the answer differs between systems, so the
/// shapes that are known to work are tried one after the other.
pub fn probe_enumeration(volume: &Volume) -> Vec<String> {
    let mut out = Vec::new();
    let _ = enable_manage_volume_privilege();

    for (label, path, flags) in [
        ("device", volume.device.clone(), 0u32),
        ("root", volume.root.clone(), FILE_FLAG_BACKUP_SEMANTICS),
    ] {
        let handle = unsafe {
            CreateFileW(
                wide(&path).as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            out.push(format!(
                "  {label}: open failed {}",
                unsafe { GetLastError() }
            ));
            continue;
        }
        for (low, high) in [(0i64, 0i64), (0, -1), (0, i64::MAX), (-1, -1)] {
            for input_size in [24usize, 40] {
                let mut request = [0u8; 40];
                request[0..8].copy_from_slice(&0u64.to_le_bytes());
                request[8..16].copy_from_slice(&low.to_le_bytes());
                request[16..24].copy_from_slice(&high.to_le_bytes());
                let mut buffer = vec![0u8; 1024 * 1024];
                let mut returned = 0u32;
                let ok = unsafe {
                    DeviceIoControl(
                        handle,
                        FSCTL_ENUM_USN_DATA,
                        request.as_ptr().cast(),
                        input_size as Dword,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as Dword,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                };
                let error = unsafe { GetLastError() };
                let mut line = format!(
                    "  {label} low={low} high={high} input={input_size} -> ok={ok} error={error} bytes={returned}"
                );
                if ok != 0 && returned >= 60 {
                    let record_length =
                        u32::from_le_bytes(buffer[0..4].try_into().unwrap());
                    let major = u16::from_le_bytes(buffer[4..6].try_into().unwrap());
                    let frn = u64::from_le_bytes(buffer[8..16].try_into().unwrap());
                    let parent = u64::from_le_bytes(buffer[16..24].try_into().unwrap());
                    let reason = u32::from_le_bytes(buffer[40..44].try_into().unwrap());
                    let attributes = u32::from_le_bytes(buffer[52..56].try_into().unwrap());
                    let name_length =
                        u16::from_le_bytes(buffer[56..58].try_into().unwrap()) as usize;
                    let name_offset =
                        u16::from_le_bytes(buffer[58..60].try_into().unwrap()) as usize;
                    let name_start = name_offset;
                    let name_end = name_start + name_length;
                    let name = if name_end <= buffer.len() && name_length % 2 == 0 {
                        let units: Vec<u16> = buffer[name_start..name_end]
                            .chunks_exact(2)
                            .map(|p| u16::from_le_bytes([p[0], p[1]]))
                            .collect();
                        String::from_utf16_lossy(&units)
                    } else {
                        String::new()
                    };
                    line.push_str(&format!(
                        "  first: len={record_length} major={major} frn={frn:#x} parent={parent:#x} reason={reason:#x} attrs={attributes:#x} name={name}"
                    ));
                }
                out.push(line);
            }
        }
        unsafe { CloseHandle(handle) };
    }
    out
}

/// The files of the volume whose change number is in the range.
///
/// The driver walks the master file table and hands out the records whose
/// number is inside the range — that is, the files that changed since the
/// last time. It is how the index is kept alive **without** the change
/// journal, which some systems refuse to read (see [`read_journal`]).
///
/// The walk costs what the walk of the whole table costs, so this is meant
/// for a round that happens now and then — not for every keystroke.
pub fn enumerate_changed_since(
    volume: &Volume,
    since: i64,
    until: i64,
    visit: impl FnMut(&str, RawEntry),
) -> Result<usize, UsnError> {
    let privilege = enable_manage_volume_privilege();
    let mut handle = unsafe {
        CreateFileW(
            wide(&volume.device).as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        handle = unsafe {
            CreateFileW(
                wide(&volume.root).as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            )
        };
    }
    if handle == INVALID_HANDLE_VALUE {
        let code = unsafe { GetLastError() };
        return Err(if code == 5 {
            UsnError::NeedsAdministrator(code)
        } else {
            UsnError::OpenFailed(code)
        });
    }
    let result = enumerate_range(handle, since, until, visit);
    unsafe { CloseHandle(handle) };
    let _ = privilege;
    result
}

/// The enumeration loop for a **range** of change numbers.
///
/// The range is not widened when the driver turns it down: the whole table
/// would come back, and the caller asked for the files that changed.
fn enumerate_range(
    handle: Handle,
    since: i64,
    until: i64,
    mut visit: impl FnMut(&str, RawEntry),
) -> Result<usize, UsnError> {
    const BUFFER: usize = 1024 * 1024;
    let mut buffer = vec![0u8; BUFFER];
    let mut scratch = String::new();
    let mut count = 0usize;
    let mut start_frn: u64 = 0;
    let mut rounds = 0u32;
    loop {
        let request = MftEnumDataV0 {
            start_file_reference_number: start_frn,
            low_usn: since,
            high_usn: until,
        };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                (&request as *const MftEnumDataV0).cast(),
                std::mem::size_of::<MftEnumDataV0>() as Dword,
                buffer.as_mut_ptr().cast(),
                buffer.len() as Dword,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        let _ = returned;
        if ok == 0 {
            let code = unsafe { GetLastError() };
            return Err(if code == 5 {
                UsnError::NeedsAdministrator(code)
            } else {
                UsnError::IoFailed(code)
            });
        }
        let mut offset = 0usize;
        let mut last_frn = start_frn;
        let mut parsed = 0usize;
        while offset + 60 <= buffer.len() {
            let Some((record, record_length)) = parse_usn_record(&buffer, offset) else {
                // Not a record here: look one byte further on.
                offset += 1;
                continue;
            };
            parsed += 1;
            let live = record.reason & (USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME) == 0;
            if live && !record.name.is_empty() {
                scratch.clear();
                push_utf16(&mut scratch, record.name);
                visit(
                    &scratch,
                    RawEntry {
                        frn: record.frn,
                        parent_frn: record.parent_frn,
                        name_start: 0,
                        name_len: scratch.len() as u32,
                        attributes: record.attributes,
                        reason: record.reason,
                        usn: record.usn as u64,
                        size: 0,
                        created: 0,
                        modified: 0,
                        accessed: 0,
                        meta_known: false,
                    },
                );
                count += 1;
            }
            if record.frn != 0 {
                last_frn = record.frn;
            }
            offset += record_length;
        }
        if offset < buffer.len() {
            buffer[offset..].fill(0);
        }
        if parsed == 0 {
            break;
        }
        start_frn = (last_frn & 0x0000_FFFF_FFFF_FFFF) + 1;
        rounds += 1;
        if rounds > 100_000 {
            break;
        }
    }
    Ok(count)
}

/// The enumeration loop, with the volume handle already open.
fn enumerate_with_handle(
    handle: Handle,
    journal_high: Option<i64>,
    progress: Option<&(dyn Fn(usize) + Sync)>,
    mut visit: impl FnMut(&str, RawEntry),
) -> Result<usize, UsnError> {
    // A megabyte at a time: the control code hands out as many records as
    // fit, and every round trip has a price.
    const BUFFER: usize = 1024 * 1024;
    let mut buffer = vec![0u8; BUFFER];
    // The name of a record is put in here and handed over; the buffer is
    // used again for the next one, so that nothing is allocated per record.
    let mut scratch = String::new();
    let mut count = 0usize;
    let mut start_frn: u64 = 0;
    // A volume with more entries than this is read in several rounds; the
    // control code continues where the previous one stopped.
    let mut rounds = 0u32;

    // The range of change numbers the control code answers for.
    //
    // The upper bound has to cover every record in the table, otherwise the
    // driver walks over the whole table again and again to find the few
    // records that fall inside the range. The next number of the change
    // journal is exactly that bound.
    const USN_RANGES: &[(i64, i64)] = &[(0, -1), (0, 0), (0, i64::MAX)];
    let mut last_error;
    let mut range = 0usize;

    loop {
        let (low, high) = match (journal_high, range) {
            // The bound from the journal covers every record, so the driver
            // never has to skip one.
            (Some(next), 0) => (0, next),
            _ => (USN_RANGES[range].0, USN_RANGES[range].1),
        };
        let request = MftEnumDataV0 {
            start_file_reference_number: start_frn,
            low_usn: low,
            high_usn: high,
        };
        let mut returned = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                (&request as *const MftEnumDataV0).cast(),
                std::mem::size_of::<MftEnumDataV0>() as Dword,
                buffer.as_mut_ptr().cast(),
                buffer.len() as Dword,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        // The count of bytes is not used: the control code writes into the
        // buffer of this process and the records carry their own length.
        let _ = returned;
        if ok == 0 {
            let code = unsafe { GetLastError() };
            last_error = code;
            // The driver refuses some ranges on some volumes. Every range is
            // tried before giving up, otherwise a volume whose journal query
            // failed would never be indexed at all.
            if range + 1 < USN_RANGES.len() {
                range += 1;
                continue;
            }
            return Err(if code == 5 {
                UsnError::NeedsAdministrator(code)
            } else {
                // ERROR_NOT_SUPPORTED (38), ERROR_INVALID_PARAMETER (87):
                // the volume is not NTFS, or the control code is not there.
                UsnError::IoFailed(last_error)
            });
        }

        // The control code works without a buffer copy, so the driver writes
        // into the memory of this process and the number of bytes it wrote
        // does not always come back. The records carry their own length, so
        // the chain is followed instead of trusting the count.
        let mut offset = 0usize;
        let mut last_frn = start_frn;
        let mut parsed = 0usize;
        while offset + 60 <= buffer.len() {
            let Some((record, record_length)) = parse_usn_record(&buffer, offset) else {
                // Not a record here: look one byte further on.
                offset += 1;
                continue;
            };
            parsed += 1;
            let live = record.reason & (USN_REASON_FILE_DELETE | USN_REASON_RENAME_OLD_NAME) == 0;
            if live && !record.name.is_empty() {
                scratch.clear();
                push_utf16(&mut scratch, record.name);
                visit(
                    &scratch,
                    RawEntry {
                        frn: record.frn,
                        parent_frn: record.parent_frn,
                        name_start: 0,
                        name_len: scratch.len() as u32,
                        attributes: record.attributes,
                        reason: record.reason,
                        usn: record.usn as u64,
                        size: 0,
                        created: 0,
                        modified: 0,
                        accessed: 0,
                        // The change journal does not carry the size or the
                        // times, so they have to be looked up later.
                        meta_known: false,
                    },
                );
                count += 1;
            }
            // A record of a version this code does not know has no number,
            // so it must not move the cursor.
            if record.frn != 0 {
                last_frn = record.frn;
            }
            offset += record_length;
        }
        // Everything behind the last record is stale and has to go, or the
        // next round would read it as if it were fresh.
        if offset < buffer.len() {
            buffer[offset..].fill(0);
        }
        if parsed == 0 {
            break;
        }

        // The control code wants the plain number of the table entry. The
        // record carries the number together with the sequence in the upper
        // half, and handing the whole thing over would make the driver look
        // for an entry that is far past the end of the table — and start
        // over from the beginning on every round.
        start_frn = (last_frn & 0x0000_FFFF_FFFF_FFFF) + 1;
        rounds += 1;
        if let Some(report) = progress {
            report(count);
        }
        if rounds > 100_000 {
            break;
        }
    }

    Ok(count)
}

/// Decode UTF-16 and push the UTF-8 form into `out`.
///
/// Surrogate pairs are joined; anything that is not a valid code point
/// becomes the replacement character, the same way
/// `String::from_utf16_lossy` would do it.
fn push_utf16(out: &mut String, bytes: &[u8]) {
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
const FILE_ID_LAYOUTS: usize = 5;

/// Build the 128 bit identifier for one layout.
fn build_file_id(layout: usize, frn: u64, entry_number: u64, sequence_number: u32) -> [u8; 16] {
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
fn open_by_file_id(volume_handle: Handle, file_id: &[u8; 16], directory: bool) -> Option<Handle> {
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
struct ByHandleFileInformation {
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
    fn GetFileInformationByHandle(
        handle: Handle,
        information: *mut ByHandleFileInformation,
    ) -> Bool;
    fn GetFinalPathNameByHandleW(
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
fn filetime_to_unix(raw: &[u8; 8]) -> u64 {
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

/// Try the ways of reading the change journal, and report what each one says.
///
/// It is the same call the program makes, but with the choices that are
/// still open: which rights the handle on the volume wants, and whether the
/// number of the journal has to be filled in.
pub fn probe_read_usn_journal(volume: &Volume) -> Vec<String> {
    let _ = enable_manage_volume_privilege();
    let state = journal_state(volume);
    let journal_id = state.as_ref().map(|s| s.usn_journal_id).unwrap_or(0);
    let next_usn = state.as_ref().map(|s| s.next_usn).unwrap_or(0);
    let mut out = Vec::new();
    out.push(format!(
        "the journal of {}: id {journal_id}, next {next_usn}",
        volume.root
    ));

    const FILE_READ_DATA: Dword = 0x0000_0001;
    const GENERIC_WRITE: Dword = 0x4000_0000;
    let accesses: &[(&str, Dword)] = &[
        ("GENERIC_READ", GENERIC_READ),
        ("GENERIC_READ|GENERIC_WRITE", GENERIC_READ | GENERIC_WRITE),
        ("FILE_READ_DATA", FILE_READ_DATA),
        (
            "the old three",
            FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE,
        ),
    ];
    let ids: &[(&str, u64)] = &[("the real number", journal_id), ("0", 0)];
    let starts: &[(&str, i64)] = &[("from the beginning", 0), ("after the newest", next_usn)];

    for (aname, access) in accesses {
        for (iname, id) in ids {
            for (sname, start) in starts {
                let handle = unsafe {
                    CreateFileW(
                        wide(&volume.root).as_ptr(),
                        *access,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        std::ptr::null_mut(),
                        OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS,
                        std::ptr::null_mut(),
                    )
                };
                if handle == INVALID_HANDLE_VALUE {
                    let error = unsafe { GetLastError() };
                    out.push(format!("{aname} / {iname} / {sname}: the handle failed, {error}"));
                    continue;
                }
                let request = ReadUsnJournalDataV1 {
                    start_usn: *start as u64,
                    reason_mask: 0xFFFF_FFFF,
                    return_only_on_close: 0,
                    timeout: 0,
                    bytes_to_wait_for: 0,
                    usn_journal_id: *id,
                    min_major_version: 2,
                    max_major_version: 3,
                };
                let mut buffer = vec![0u8; 1024 * 1024];
                let mut returned = 0u32;
                let ok = unsafe {
                    DeviceIoControl(
                        handle,
                        FSCTL_READ_USN_JOURNAL,
                        (&request as *const ReadUsnJournalDataV1).cast(),
                        std::mem::size_of::<ReadUsnJournalDataV1>() as Dword,
                        buffer.as_mut_ptr().cast(),
                        buffer.len() as Dword,
                        &mut returned,
                        std::ptr::null_mut(),
                    )
                };
                unsafe { CloseHandle(handle) };
                if ok == 0 {
                    let error = unsafe { GetLastError() };
                    out.push(format!("{aname} / {iname} / {sname}: it could not be read, {error}"));
                    continue;
                }
                buffer.truncate(returned as usize);
                let mut count = 0usize;
                let mut offset = 0usize;
                while offset + USN_RECORD_V2_HEADER <= buffer.len() {
                    let Some((_record, length)) = parse_usn_record(&buffer, offset) else {
                        offset += 1;
                        continue;
                    };
                    count += 1;
                    offset += length;
                }
                out.push(format!(
                    "{aname} / {iname} / {sname}: {count} records, {returned} bytes"
                ));
            }
        }
    }
    out
}

/// The file reference number of the file, and the path of it.
///
/// Everything the launcher needs about a file that the change journal only
/// mentions by its number.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversion_matches_the_unix_epoch() {
        let ticks = 116_444_736_000_000_000u64.to_le_bytes();
        assert_eq!(filetime_to_unix(&ticks), 0);
    }

    #[test]
    fn the_file_id_is_built_in_every_layout() {
        // Every layout has to fit into the sixteen bytes of the identifier.
        // A layout that does not is a panic in the middle of a query.
        for layout in 0..FILE_ID_LAYOUTS {
            let id = build_file_id(layout, 0x1234_5678_9ABC_DEF0, 0x9ABC_DEF0, 0x1234);
            assert_eq!(id.len(), 16);
        }
    }

    #[test]
    fn attributes_are_classified() {
        assert!(is_hidden_by_default(FILE_ATTRIBUTE_HIDDEN));
        assert!(!is_hidden_by_default(FILE_ATTRIBUTE_DIRECTORY));
        assert!(is_reparse_point(FILE_ATTRIBUTE_REPARSE_POINT));
        assert!(is_read_only(FILE_ATTRIBUTE_READONLY));
    }

    #[test]
    fn parent_map_links_children_to_their_folder() {
        let entries = vec![
            UsnEntry {
                frn: 5,
                parent_frn: 5,
                name: String::new(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
            },
            UsnEntry {
                frn: 100,
                parent_frn: 5,
                name: "Windows".to_string(),
                attributes: FILE_ATTRIBUTE_DIRECTORY,
            },
            UsnEntry {
                frn: 101,
                parent_frn: 100,
                name: "notepad.exe".to_string(),
                attributes: 0,
            },
        ];
        let map = build_parent_map(&entries);
        assert_eq!(map.get(&101), Some(&100));
        assert_eq!(map.get(&100), Some(&5));
    }

    /// Build a `USN_RECORD_V2` the way the driver would.
    fn build_record(
        frn: u64,
        parent_frn: u64,
        reason: u32,
        attributes: u32,
        name: &str,
    ) -> Vec<u8> {
        let name_units: Vec<u16> = name.encode_utf16().collect();
        let name_bytes: Vec<u8> = name_units
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let record_length = 60 + name_bytes.len();
        let mut record = vec![0u8; record_length];
        record[0..4].copy_from_slice(&(record_length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&2u16.to_le_bytes());
        record[6..8].copy_from_slice(&0u16.to_le_bytes());
        record[8..16].copy_from_slice(&frn.to_le_bytes());
        record[16..24].copy_from_slice(&parent_frn.to_le_bytes());
        record[24..32].copy_from_slice(&0i64.to_le_bytes());
        record[32..40].copy_from_slice(&0i64.to_le_bytes());
        record[40..44].copy_from_slice(&reason.to_le_bytes());
        record[44..48].copy_from_slice(&0u32.to_le_bytes());
        record[48..52].copy_from_slice(&0u32.to_le_bytes());
        record[52..56].copy_from_slice(&attributes.to_le_bytes());
        record[56..58].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        record[58..60].copy_from_slice(&60u16.to_le_bytes());
        record[60..].copy_from_slice(&name_bytes);
        record
    }

    /// Build a `USN_RECORD_V3` the way the driver would.
    ///
    /// This is the shape the journal has on a volume with 128 bit file
    /// numbers: the two file numbers are sixteen bytes wide, and everything
    /// behind them moves sixteen bytes to the right.
    fn build_record_v3(
        frn: u64,
        parent_frn: u64,
        usn: i64,
        reason: u32,
        attributes: u32,
        name: &str,
    ) -> Vec<u8> {
        let name_units: Vec<u16> = name.encode_utf16().collect();
        let name_bytes: Vec<u8> = name_units
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let record_length = 76 + name_bytes.len();
        let mut record = vec![0u8; record_length];
        record[0..4].copy_from_slice(&(record_length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&3u16.to_le_bytes());
        record[6..8].copy_from_slice(&0u16.to_le_bytes());
        // The 128 bit number: the ordinary number sits in the low half.
        record[8..16].copy_from_slice(&frn.to_le_bytes());
        record[16..24].copy_from_slice(&0u64.to_le_bytes());
        record[24..32].copy_from_slice(&parent_frn.to_le_bytes());
        record[32..40].copy_from_slice(&0u64.to_le_bytes());
        record[40..48].copy_from_slice(&usn.to_le_bytes());
        record[48..56].copy_from_slice(&0i64.to_le_bytes());
        record[56..60].copy_from_slice(&reason.to_le_bytes());
        record[60..64].copy_from_slice(&0u32.to_le_bytes());
        record[64..68].copy_from_slice(&0u32.to_le_bytes());
        record[68..72].copy_from_slice(&attributes.to_le_bytes());
        record[72..74].copy_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        record[74..76].copy_from_slice(&76u16.to_le_bytes());
        record[76..].copy_from_slice(&name_bytes);
        record
    }

    /// Decode the UTF-16 name of a parsed record.
    fn decode(name: &[u8]) -> String {
        let mut out = String::new();
        push_utf16(&mut out, name);
        out
    }

    /// Read every record of a buffer, the way the program reads the journal.
    fn read_all(buffer: &[u8]) -> Vec<(RawRecord<'_>, usize)> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        while offset + USN_RECORD_V2_HEADER <= buffer.len() {
            let Some((record, length)) = parse_usn_record(buffer, offset) else {
                offset += 1;
                continue;
            };
            out.push((record, length));
            offset += length;
        }
        out
    }

    #[test]
    fn usn_record_is_parsed_field_by_field() {
        let buffer = build_record(0x1234, 0x40, 0x8000_0000, 0x20, "report.pdf");
        let (record, length) = parse_usn_record(&buffer, 0).expect("a record");
        assert_eq!(length, buffer.len());
        assert_eq!(record.frn, 0x1234);
        assert_eq!(record.parent_frn, 0x40);
        assert_eq!(record.attributes, 0x20);
        assert_eq!(decode(record.name), "report.pdf");
    }

    #[test]
    fn usn_record_handles_a_chinese_name() {
        let buffer = build_record(7, 5, 0x8000_0000, 0, "季度报告.xlsx");
        let (record, _) = parse_usn_record(&buffer, 0).expect("a record");
        assert_eq!(decode(record.name), "季度报告.xlsx");
    }

    #[test]
    fn usn_records_are_read_one_after_the_other() {
        let mut buffer = build_record(10, 5, 0x8000_0000, 0, "a.txt");
        buffer.extend(build_record(11, 5, 0x8000_0000, 0, "b.txt"));
        let (first, first_len) = parse_usn_record(&buffer, 0).expect("the first");
        let (second, _) = parse_usn_record(&buffer, first_len).expect("the second");
        assert_eq!(decode(first.name), "a.txt");
        assert_eq!(decode(second.name), "b.txt");
        assert_eq!(second.frn, 11);
    }

    #[test]
    fn a_truncated_record_is_rejected() {
        let mut buffer = build_record(10, 5, 0x8000_0000, 0, "a.txt");
        buffer.truncate(40);
        assert!(parse_usn_record(&buffer, 0).is_none());
    }

    #[test]
    fn the_record_with_a_128_bit_number_is_parsed() {
        // The journal of this machine writes the newer shape, and the older
        // parser understood none of it.
        let buffer = build_record_v3(
            0x0002_0000_007A_A710,
            0x0002_0000_0078_8872,
            38_990_249_984,
            0x8000,
            0x21,
            "eadc078028ebf79e2a2bf8c25216f84fddf0fc",
        );
        let (record, length) = parse_usn_record(&buffer, 0).expect("a record");
        assert_eq!(length, buffer.len());
        assert_eq!(record.frn, 0x0002_0000_007A_A710);
        assert_eq!(record.parent_frn, 0x0002_0000_0078_8872);
        assert_eq!(record.usn, 38_990_249_984);
        assert_eq!(record.reason, 0x8000);
        assert_eq!(record.attributes, 0x21);
        assert_eq!(
            decode(record.name),
            "eadc078028ebf79e2a2bf8c25216f84fddf0fc"
        );
    }

    #[test]
    fn the_buffer_may_begin_with_the_tail_of_the_previous_record() {
        // The driver can hand out the last few bytes of the record that came
        // before the one that was asked for. The reader has to find the first
        // whole record anyway.
        let mut buffer = vec![0x00, 0x60, 0x0E, 0x14, 0x09, 0x00, 0x00, 0x00];
        buffer.extend(build_record_v3(11, 5, 42, 0x8000, 0x20, "b.txt"));
        let records = read_all(&buffer);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0.frn, 11);
        assert_eq!(decode(records[0].0.name), "b.txt");
    }

    #[test]
    fn a_reparse_point_is_recognised() {
        assert!(is_reparse_point(FILE_ATTRIBUTE_REPARSE_POINT));
        assert!(!is_reparse_point(FILE_ATTRIBUTE_DIRECTORY));
    }
}
