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
const GENERIC_WRITE: Dword = 0x4000_0000;
const FILE_SHARE_READ: Dword = 0x0000_0001;
const FILE_SHARE_WRITE: Dword = 0x0000_0002;
const OPEN_EXISTING: Dword = 3;

const DRIVE_FIXED: u32 = 3;

/// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 44, METHOD_BUFFERED, FILE_ANY_ACCESS)`
const FSCTL_ENUM_USN_DATA: Dword = 0x0009_00B3;
/// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 40, METHOD_BUFFERED, FILE_ANY_ACCESS)`
const FSCTL_QUERY_USN_JOURNAL: Dword = 0x0009_00F4;
const FSCTL_READ_USN_JOURNAL: Dword = 0x0009_00BB;
/// `CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 57, METHOD_BUFFERED, FILE_ANY_ACCESS)`
///
/// 把一个没有变更日志的卷（`ERROR_JOURNAL_NOT_ACTIVE`，1179）变成有日志的卷。
/// 只有带着 `GENERIC_WRITE` 的句柄、并且拿着「管理卷」的特权才能调用。
const FSCTL_CREATE_USN_JOURNAL: Dword = 0x0009_00E7;

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


// Entry points live in child modules by concern; each child sees the private
// helpers through `super`, which is what keeps this split a pure movement.
mod win32;
mod mft;
mod journal;
mod privilege;
mod metadata;

pub use win32::{UsnJournalDataV0};
pub use mft::{collect_raw, enumerate_changed_since, enumerate_mft, enumerate_mft_with, enumerate_mft_with_bytes, enumerate_volume, enumerate_volume_with, probe_enumeration};
pub use journal::{JournalChange, JournalRecovery, count_raw_records, create_journal, describe_raw_records, describe_records_matching, journal_recovery_plan, journal_state, journal_state_at, probe_control_codes, probe_read_usn_journal, read_journal, read_journal_raw};
pub use privilege::{PRIVILEGE_MANAGE_VOLUME, PrivilegeOutcome, enable_manage_volume_privilege, enable_manage_volume_privilege_detailed, enable_privilege, is_elevated, token_privilege_bytes, token_privileges};
pub use metadata::{build_parent_map, file_identity, file_metadata, is_hidden_by_default, is_read_only, is_reparse_point, volume_root};
pub use journal::reason;

pub use crate::mft::RecordFields;
pub(in crate::usn) use win32::*;
pub(in crate::usn) use journal::*;
pub(in crate::usn) use metadata::*;

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

    /// A journal that stands with its ring between these two numbers.
    fn journal_state_fixture() -> UsnJournalDataV0 {
        UsnJournalDataV0 {
            usn_journal_id: 0x1234,
            first_usn: 1_000_000,
            next_usn: 9_000_000,
            lowest_valid_usn: 1_000_000,
            max_usn: 0,
            maximum_size: 0,
            allocation_delta: 0,
        }
    }

    #[test]
    fn a_volume_without_a_journal_gets_one_made() {
        // 1179 with a query that also failed: making the journal is the only
        // way a volume like this ever becomes followable.
        assert_eq!(
            journal_recovery_plan(1179, None, 0),
            JournalRecovery::CreateJournal
        );
    }

    #[test]
    fn a_read_that_disagrees_with_a_working_query_anchors_on_the_query() {
        // 1179 but the query says the journal is there: trust the query and
        // go on from its newest record.
        let state = journal_state_fixture();
        assert_eq!(
            journal_recovery_plan(1179, Some(state.clone()), 0),
            JournalRecovery::Reanchor(state.next_usn)
        );
    }

    #[test]
    fn a_place_that_the_ring_has_recycled_needs_a_rebuild() {
        // 1181 with the place behind the oldest record still in the ring:
        // the gap cannot be read from anywhere.
        let state = journal_state_fixture();
        assert_eq!(
            journal_recovery_plan(1181, Some(state.clone()), 500_000),
            JournalRecovery::Rebuild
        );
        // Exactly on the line is already outside — the records start at the
        // first one.
        assert_eq!(
            journal_recovery_plan(1181, Some(state.clone()), state.first_usn - 1),
            JournalRecovery::Rebuild
        );
    }

    #[test]
    fn a_place_inside_the_ring_is_anchored_on_the_newest_record() {
        // 1181 with a place that is actually still in the ring: the failure
        // came from something else, and going on from the newest record is
        // the fastest way back.
        let state = journal_state_fixture();
        assert_eq!(
            journal_recovery_plan(1181, Some(state.clone()), 2_000_000),
            JournalRecovery::Reanchor(state.next_usn)
        );
    }

    #[test]
    fn a_failed_query_during_a_deleted_entry_is_a_plain_retry() {
        // 1181 without a state to decide from: retrying costs nothing.
        assert_eq!(journal_recovery_plan(1181, None, 0), JournalRecovery::Retry);
    }

    #[test]
    fn codes_this_module_does_not_decide_about_are_retried() {
        // Access rights the process does not have, and whatever else the
        // system can say: not this module's call to make.
        assert_eq!(journal_recovery_plan(5, None, 0), JournalRecovery::Retry);
        let state = journal_state_fixture();
        assert_eq!(
            journal_recovery_plan(1784, Some(state), 0),
            JournalRecovery::Retry
        );
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