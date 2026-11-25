use super::*;


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
            // The query wants the handle to **read** the volume, just like
            // the read of the records does (`FSCTL_QUERY_USN_JOURNAL` checks
            // the rights the handle carries, not the ones the process has).
            // This used to open with `FILE_READ_ATTRIBUTES`, the query was
            // turned down, and every caller of this function then fell back
            // to zero — the place to read from, and the number of the
            // journal itself. Reading from zero fails with
            // `ERROR_JOURNAL_ENTRY_DELETED` (1181) for ever, which is what
            // kept the whole-disk index from ever following the changes.
            GENERIC_READ,
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

/// The input of `FSCTL_CREATE_USN_JOURNAL`.
///
/// Both fields at zero tell the file system to pick its own sizes, which is
/// what `fsutil usn createjournal` does when the numbers are left out.
#[repr(C)]
pub(in crate::usn) struct CreateUsnJournalData {
    maximum_size: u64,
    allocation_delta: u64,
}

/// Make the change journal of a volume that has none.
///
/// A volume without a journal (`ERROR_JOURNAL_NOT_ACTIVE`, 1179) can never
/// be followed — until one is made. The call needs the handle to open the
/// volume **for writing** and the「管理卷」privilege, which is why it lives
/// behind the same privilege dance as the reading of the records.
///
/// The journal starts empty, so the place to read from afterwards is the
/// `next_usn` that [`journal_state`] reports right after this call returns.
pub fn create_journal(volume: &Volume) -> Result<(), u32> {
    let _ = enable_manage_volume_privilege();
    let handle = unsafe {
        CreateFileW(
            wide(&volume.root).as_ptr(),
            // Creating the journal is a write to the volume, so the handle
            // has to carry the write right — the read right of the other
            // calls is not enough here.
            GENERIC_READ | GENERIC_WRITE,
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
    let request = CreateUsnJournalData {
        maximum_size: 0,
        allocation_delta: 0,
    };
    let mut returned = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_CREATE_USN_JOURNAL,
            (&request as *const CreateUsnJournalData).cast(),
            std::mem::size_of::<CreateUsnJournalData>() as Dword,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Err(unsafe { GetLastError() });
    }
    Ok(())
}

/// What to do after a round could not read the change journal.
///
/// The codes are the ones the system hands back through
/// [`read_journal_raw`]; the plan is decided from the code and the state of
/// the journal as it stands **now** — the state that is queried fresh for
/// every decision, not a copy that might be stale.
#[derive(Debug, PartialEq, Eq)]
pub enum JournalRecovery {
    /// Log it and try the same place again — the right answer when the
    /// failure looks like nothing this module can fix (an access right the
    /// process does not have, for instance).
    Retry,
    /// The journal is there but the place is not in it any more, or there is
    /// no place yet: go on from the newest record. Changes that happened
    /// while the place was stale are lost — the caller has to make up for
    /// that in its own way (this is also why the plan is only acted on when
    /// the failures have stacked up).
    Reanchor(i64),
    /// The volume has no journal at all: make one, then go on from the
    /// newest record.
    CreateJournal,
    /// The place has fallen out of the ring and the gap cannot be covered:
    /// the table has to be read off the disk again.
    Rebuild,
}

/// Decide what to do about a change journal that could not be read.
///
/// The three codes this actually tells apart:
///
/// * `ERROR_JOURNAL_NOT_ACTIVE` (1179) — the volume has no journal. Making
///   one is the fix; from then on the volume is followable.
/// * `ERROR_JOURNAL_ENTRY_DELETED` (1181) — the place that was asked for is
///   older than the oldest record still in the ring. The records in between
///   are gone, so no reading can cover the gap: the table is read again.
/// * anything else — not this module's call to make. Retry with the same
///   place, and let the log say which code it is.
pub fn journal_recovery_plan(
    code: u32,
    state: Option<UsnJournalDataV0>,
    point: i64,
) -> JournalRecovery {
    match code {
        1179 => match state {
            // The query and the read disagree: the query says there is a
            // journal, the read says there is not. Trust the query and go on
            // from where it says the journal stands.
            Some(state) => JournalRecovery::Reanchor(state.next_usn),
            None => JournalRecovery::CreateJournal,
        },
        1181 => {
            let Some(state) = state else {
                // The query itself failed — nothing to decide from. Trying
                // the same place again costs nothing.
                return JournalRecovery::Retry;
            };
            if point < state.first_usn {
                // Everything before `first_usn` has been recycled, and the
                // place is behind that line. The stretch in between cannot
                // be read from anywhere — the table has to be built again
                // from the master file table, which also writes a fresh
                // place into itself.
                JournalRecovery::Rebuild
            } else {
                // The place is inside the ring after all: the failure came
                // from somewhere else (a wrong number of the journal, for
                // instance). Going on from the newest record is the fastest
                // way back.
                JournalRecovery::Reanchor(state.next_usn)
            }
        }
        _ => JournalRecovery::Retry,
    }
}

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
pub(in crate::usn) struct ReadUsnJournalDataV1 {
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
pub(in crate::usn) fn decode_utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16_lossy(&units)
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
pub(in crate::usn) struct RawRecord<'a> {
    pub(in crate::usn) frn: u64,
    pub(in crate::usn) parent_frn: u64,
    /// The number of the change that produced this record.
    pub(in crate::usn) usn: i64,
    pub(in crate::usn) reason: u32,
    pub(in crate::usn) attributes: u32,
    /// The name, still in UTF-16.
    pub(in crate::usn) name: &'a [u8],
}

/// The size of the header of a `USN_RECORD_V2`.
pub(in crate::usn) const USN_RECORD_V2_HEADER: usize = 60;
/// The size of the header of a `USN_RECORD_V3`.
pub(in crate::usn) const USN_RECORD_V3_HEADER: usize = 76;

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
pub(in crate::usn) fn parse_usn_record(
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