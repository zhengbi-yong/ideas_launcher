use super::*;


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
pub(in crate::usn) fn enumerate_range(
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
pub(in crate::usn) fn enumerate_with_handle(
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