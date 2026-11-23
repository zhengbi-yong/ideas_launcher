//! Reading the NTFS master file table straight off the disk.
//!
//! Everything does it this way, and the debug strings of the signed 1.4.1
//! release show the steps (see `analysis/EVERYTHING-REPORT.md` §2.1):
//!
//! | string | meaning |
//! |---|---|
//! | `bad NTFS signature: %t` | the boot sector is checked first |
//! | `mft_lcn %I64u` | the boot sector says where the table starts |
//! | `FSCTL_GET_NTFS_VOLUME_DATA failed %u` | the same thing over a control code |
//! | `MFT file_size %I64u` | the size of the table |
//! | `MFT mapping_pairs_offset %u` | where the list of extents starts |
//! | `found extended MFT file record %016I64x base %016I64x` | records whose attributes live elsewhere |
//!
//! The advantage over asking the driver for the records
//! (`FSCTL_ENUM_USN_DATA`) is that the table is read **once, in order**,
//! instead of the driver walking it again for every slice of it. On this
//! machine that is the difference between 383 seconds and a second.
//!
//! The read needs administrative privileges: the volume is opened as a raw
//! device and read at sector offsets.

#![cfg(windows)]

use crate::usn::Volume;

/// The type of the attribute that holds the standard information.
const ATTR_STANDARD_INFORMATION: u32 = 0x10;
/// The type of the attribute that lists the other attributes.
const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
/// The type of the attribute that holds the file name.
const ATTR_FILE_NAME: u32 = 0x30;
/// The type of the attribute that holds the content.
const ATTR_DATA: u32 = 0x80;
/// The type that ends the list of attributes.
const ATTR_END: u32 = 0xFFFF_FFFF;

/// The raw window handles and calls.
///
/// The module repeats a few of the declarations of `usn` on purpose: the two
/// read the disk in very different ways and keeping them apart makes each of
/// them easier to follow.
mod sys {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;
    pub type Bool = i32;
    pub type Dword = u32;

    #[repr(C)]
    pub struct SecurityAttributes {
        pub length: u32,
        pub descriptor: *mut c_void,
        pub inherit_handle: Bool,
    }

    unsafe extern "system" {
        pub fn CreateFileW(
            file_name: *const u16,
            desired_access: Dword,
            share_mode: Dword,
            security_attributes: *mut SecurityAttributes,
            creation_disposition: Dword,
            flags_and_attributes: Dword,
            template_file: Handle,
        ) -> Handle;
        pub fn CloseHandle(object: Handle) -> Bool;
        pub fn ReadFile(
            file: Handle,
            buffer: *mut c_void,
            bytes_to_read: Dword,
            bytes_read: *mut Dword,
            overlapped: *mut c_void,
        ) -> Bool;
        pub fn SetFilePointerEx(
            file: Handle,
            distance: i64,
            new_pointer: *mut i64,
            method: Dword,
        ) -> Bool;
        pub fn GetLastError() -> Dword;
    }
}

/// Why the table could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MftError {
    /// The volume could not be opened as a raw device.
    OpenFailed(u32),
    /// The volume does not look like NTFS.
    NotNtfs,
    /// The boot sector could not be read.
    ReadFailed(u32),
    /// The table has no extents.
    NoExtents(String),
}

impl std::fmt::Display for MftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MftError::OpenFailed(code) => write!(f, "the volume could not be opened ({code})"),
            MftError::NotNtfs => write!(f, "the volume is not NTFS"),
            MftError::ReadFailed(code) => write!(f, "the disk could not be read ({code})"),
            MftError::NoExtents(details) => {
                write!(f, "the master file table has no extents: {details}")
            }
        }
    }
}

impl std::error::Error for MftError {}

/// One record of the table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MftRecord {
    /// The number of the record in the table.
    pub number: u64,
    /// The sequence number, the upper half of the file reference number.
    pub sequence: u16,
    /// The name of the file or folder.
    pub name: String,
    /// The file reference number of the folder that holds it.
    pub parent: u64,
    /// The size of the content.
    pub size: u64,
    /// When the file was created, in seconds since the Unix epoch.
    pub created: u64,
    /// When the content changed the last time.
    pub modified: u64,
    /// When the file was read the last time.
    pub accessed: u64,
    /// True for folders.
    pub is_directory: bool,
}

impl MftRecord {
    /// The file reference number of the entry.
    ///
    /// The lower half is the number of the record in the table, the upper half
    /// is the sequence number.
    pub fn file_reference(&self) -> u64 {
        (self.sequence as u64) << 48 | (self.number & 0x0000_FFFF_FFFF_FFFF)
    }
}

/// One run of clusters of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    /// The first cluster of the run on the disk.
    lcn: u64,
    /// How many clusters the run holds.
    clusters: u64,
    /// Where the run starts in the table, in bytes.
    logical: u64,
}

/// The fields of one record, without the name.
///
/// The name is handed over separately, because it is the one part that has to
/// be turned into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordFields {
    /// The number of the record in the table.
    pub number: u64,
    /// The sequence number, the upper half of the file reference number.
    pub sequence: u16,
    /// The number of the record of the folder that holds it.
    ///
    /// The table stores the 48 bit number of the record, not the whole file
    /// reference number, so the folder is looked up by that number.
    pub parent: u64,
    /// The size of the content.
    pub size: u64,
    /// When the file was created, in seconds since the Unix epoch.
    pub created: u64,
    /// When the content changed the last time.
    pub modified: u64,
    /// When the file was read the last time.
    pub accessed: u64,
    /// True for folders.
    pub is_directory: bool,
    /// True when the attributes of the record continue in other records.
    ///
    /// Such a record holds a list of the places where the rest of its
    /// attributes live. The name is still in the record itself, but the size
    /// of the content may be missing.
    pub split: bool,
}

/// What the walk over the table found out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MftStats {
    /// How many records were read.
    pub records: u64,
    /// How many bytes were read from the disk.
    pub bytes: u64,
    /// How long the disk took, in milliseconds.
    pub read_ms: u64,
    /// How long the parsing took, in milliseconds.
    pub parse_ms: u64,
    /// How many records at the end of the table were empty.
    ///
    /// The table is allocated in pieces, so it is usually longer than the
    /// part that is in use. The walk stops once a long run of empty records
    /// shows that the rest of the table is not in use.
    pub empty_tail: u64,
    /// How many records had to be looked for in the records that hold the
    /// attributes a record could not hold itself.
    pub extensions: u64,
    /// The error of the read that stopped the walk, if there was one.
    pub error: u32,
}

/// The master file table of one volume.
pub struct MftReader {
    handle: sys::Handle,
    /// The handle the table itself is read with.
    ///
    /// The read bypasses the cache: the table is nine gigabytes, and putting
    /// that into the cache would push everything else out of it. A read that
    /// bypasses the cache has to be aligned to the size of a sector, so the
    /// buffer for it is allocated on its own.
    raw_handle: sys::Handle,
    bytes_per_sector: u32,
    sectors_per_cluster: u32,
    record_size: u32,
    /// The places the table lives on the disk: the first cluster and how many
    /// clusters it has.
    extents: Vec<Extent>,
    /// How many records the table holds.
    record_count: u64,
    /// The cluster where the table starts.
    mft_lcn: u64,
    /// The size of a record as the boot sector says it.
    record_size_from_boot: u32,
}

impl Drop for MftReader {
    fn drop(&mut self) {
        unsafe { sys::CloseHandle(self.handle) };
        unsafe { sys::CloseHandle(self.raw_handle) };
    }
}

impl MftReader {
    /// Open the table of a volume.
    pub fn open(volume: &Volume) -> Result<Self, MftError> {
        let handle = unsafe {
            sys::CreateFileW(
                wide(&volume.device).as_ptr(),
                0x8000_0000, // GENERIC_READ
                0x0000_0001 | 0x0000_0002, // FILE_SHARE_READ | FILE_SHARE_WRITE
                std::ptr::null_mut(),
                3, // OPEN_EXISTING
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == -1isize as sys::Handle {
            return Err(MftError::OpenFailed(unsafe { sys::GetLastError() }));
        }
        // The handle the table is read with. The read goes straight to the
        // disk, so the buffer for it has to be aligned to the sector.
        let raw_handle = unsafe {
            sys::CreateFileW(
                wide(&volume.device).as_ptr(),
                0x8000_0000, // GENERIC_READ
                0x0000_0001 | 0x0000_0002, // FILE_SHARE_READ | FILE_SHARE_WRITE
                std::ptr::null_mut(),
                3, // OPEN_EXISTING
                0x0800_0000 | 0x2000_0000, // SEQUENTIAL_SCAN | NO_BUFFERING
                std::ptr::null_mut(),
            )
        };
        if raw_handle == -1isize as sys::Handle {
            unsafe { sys::CloseHandle(handle) };
            return Err(MftError::OpenFailed(unsafe { sys::GetLastError() }));
        }

        // ---- the boot sector ------------------------------------------------
        let mut boot = [0u8; 512];
        if !unsafe { read_at(handle, 0, &mut boot) } {
            let error = unsafe { sys::GetLastError() };
            unsafe { sys::CloseHandle(handle) };
            unsafe { sys::CloseHandle(raw_handle) };
            return Err(MftError::ReadFailed(error));
        }
        if &boot[3..11] != b"NTFS    " {
            unsafe { sys::CloseHandle(handle) };
            unsafe { sys::CloseHandle(raw_handle) };
            return Err(MftError::NotNtfs);
        }
        let bytes_per_sector = u16::from_le_bytes(boot[0x0B..0x0D].try_into().unwrap()) as u32;
        let sectors_per_cluster = boot[0x0D] as u32;
        // The boot sector of NTFS holds the number of the cluster where the
        // table starts at 0x30, and the mirror of the table at 0x38. The two
        // are the same on most volumes, but they are not the same field, so
        // both are tried and the one that really holds the table is used.
        let mft_candidates = [
            u64::from_le_bytes(boot[0x30..0x38].try_into().unwrap()),
            u64::from_le_bytes(boot[0x38..0x40].try_into().unwrap()),
        ];
        // The size of a record is stored as a signed byte: a positive one
        // counts clusters, a negative one is a power of two in bytes.
        let record_clusters = boot[0x40] as i8;
        let record_size_from_boot = if record_clusters > 0 {
            (record_clusters as u32) * sectors_per_cluster * bytes_per_sector
        } else {
            // The number of the power of two is at most 31, so that the shift
            // stays in range.
            1u32 << ((-record_clusters).clamp(0, 31) as u32)
        };

        // ---- the record of the table itself ---------------------------------
        let cluster_size = sectors_per_cluster * bytes_per_sector;
        // The first record is read with room to spare: the size of a record
        // is worked out from it afterwards.
        let mut first = vec![0u8; 64 * 1024];
        let mut mft_lcn = 0u64;
        let mut record_size = record_size_from_boot;
        let mut found = false;
        for candidate in mft_candidates {
            let offset = candidate * cluster_size as u64;
            if !unsafe { read_at(handle, offset as i64, &mut first) } {
                continue;
            }
            if &first[0..4] != b"FILE" {
                continue;
            }
            // The update sequence array holds one value per sector of the
            // record, so it says how long the record is.
            let usa_count = u16::from_le_bytes(first[0x06..0x08].try_into().unwrap()) as u32;
            if (2..=64).contains(&usa_count) {
                let from_usa = (usa_count - 1) * 512;
                if (512..=64 * 1024).contains(&from_usa) {
                    record_size = from_usa;
                }
            }
            mft_lcn = candidate;
            found = true;
            break;
        }
        if !found {
            let error = unsafe { sys::GetLastError() };
            unsafe { sys::CloseHandle(handle) };
            unsafe { sys::CloseHandle(raw_handle) };
            return Err(MftError::ReadFailed(error));
        }
        let record = first[..record_size as usize].to_vec();

        // The unnamed data attribute of the first record is the table itself.
        // Some volumes give it a name, so a named one is used as a fallback.
        let mut fixed = record.clone();
        fix_up(&mut fixed);
        let attributes_offset =
            u16::from_le_bytes(fixed[0x14..0x16].try_into().unwrap()) as usize;
        let mut runs = data_runs_of(&fixed, attributes_offset);
        if runs.is_empty() {
            walk_attributes(&fixed, attributes_offset, |attribute| {
                if attribute.kind == ATTR_DATA && runs.is_empty() && !attribute.runs.is_empty() {
                    runs = parse_runs(attribute.runs);
                }
            });
        }
        if runs.is_empty() {
            // The reason is carried along, so that the probe can show what is
            // wrong with the record.
            let mut lines = vec![
                format!(
                    "signature={:?} usa_offset={} usa_count={} attributes_offset={:#x}",
                    String::from_utf8_lossy(&record[0..4]),
                    u16::from_le_bytes(record[0x04..0x06].try_into().unwrap()),
                    u16::from_le_bytes(record[0x06..0x08].try_into().unwrap()),
                    attributes_offset
                ),
            ];
            walk_attributes(&fixed, attributes_offset, |attribute| {
                lines.push(format!(
                    "attribute kind={:#x} non_resident={} name={:?} value={} data_size={:?} runs={}",
                    attribute.kind,
                    attribute.non_resident,
                    attribute.name(),
                    attribute.value.len(),
                    attribute.data_size,
                    attribute.runs.len()
                ));
            });
            unsafe { sys::CloseHandle(handle) };
            unsafe { sys::CloseHandle(raw_handle) };
            return Err(MftError::NoExtents(lines.join(" | ")));
        }

        // The number of records: the size of the table divided by the size of
        // one record.
        let total_clusters: u64 = runs.iter().map(|(_, count)| *count).sum();
        let record_count = total_clusters * cluster_size as u64 / record_size as u64;
        let extents: Vec<Extent> = {
            let mut out = Vec::with_capacity(runs.len());
            let mut logical = 0u64;
            for (lcn, clusters) in runs {
                out.push(Extent {
                    lcn,
                    clusters,
                    logical,
                });
                logical += clusters * cluster_size as u64;
            }
            out
        };

        Ok(Self {
            handle,
            raw_handle,
            bytes_per_sector,
            sectors_per_cluster,
            record_size,
            extents,
            record_count,
            mft_lcn,
            record_size_from_boot,
        })
    }

    /// How many records the table holds.
    pub fn record_count(&self) -> u64 {
        self.record_count
    }

    /// Describe how the table was read.
    ///
    /// This is what the probe prints, so that the reason for a failure can be
    /// seen without a debugger. The raw bytes of the interesting parts are
    /// printed as well: the header of the content attribute and the value of
    /// the name attribute.
    pub fn debug(&self) -> Vec<String> {
        let mut out = Vec::new();
        out.push(format!(
            "  sector={} cluster={} record={} (boot says {}) records={} extents={} mft_lcn={}",
            self.bytes_per_sector,
            self.sectors_per_cluster,
            self.record_size,
            self.record_size_from_boot,
            self.record_count,
            self.extents.len(),
            self.mft_lcn
        ));
        for (index, extent) in self.extents.iter().enumerate().take(16) {
            out.push(format!(
                "    extent {index}: lcn={} clusters={} logical={}",
                extent.lcn, extent.clusters, extent.logical
            ));
        }
        let Some(offset) = self.offset_of(0) else {
            out.push("  the first record is not on the disk".to_string());
            return out;
        };
        let mut buffer = vec![0u8; self.record_size as usize];
        if !unsafe { read_at(self.handle, offset as i64, &mut buffer) } {
            out.push("  the first record could not be read".to_string());
            return out;
        }
        out.push(format!(
            "  record 0 at offset {offset}: {}",
            hex_of(&buffer[..64.min(buffer.len())])
        ));
        out.push(format!(
            "  signature={:?} usa_offset={} usa_count={} attributes_offset={} flags={:#x}",
            String::from_utf8_lossy(&buffer[0..4]),
            u16::from_le_bytes(buffer[0x04..0x06].try_into().unwrap()),
            u16::from_le_bytes(buffer[0x06..0x08].try_into().unwrap()),
            u16::from_le_bytes(buffer[0x14..0x16].try_into().unwrap()),
            u16::from_le_bytes(buffer[0x16..0x18].try_into().unwrap()),
        ));
        let mut fixed = buffer.clone();
        fix_up(&mut fixed);
        let attributes_offset = u16::from_le_bytes(fixed[0x14..0x16].try_into().unwrap()) as usize;
        walk_attributes(&fixed, attributes_offset, |attribute| {
            out.push(format!(
                "  attribute kind={:#x} non_resident={} name={:?} value={} data_size={:?} runs={}",
                attribute.kind,
                attribute.non_resident,
                attribute.name(),
                attribute.value.len(),
                attribute.data_size,
                attribute.runs.len()
            ));
            if attribute.kind == ATTR_DATA {
                // The header of a content attribute that lives outside the
                // record: the places of the size and of the list of extents
                // are the interesting ones.
                let start = attribute.start;
                let end = (start + 0x40).min(fixed.len());
                out.push(format!("    header: {}", hex_of(&fixed[start..end])));
                let mut sizes = Vec::new();
                for at in [0x28usize, 0x30, 0x38] {
                    if start + at + 8 > fixed.len() {
                        continue;
                    }
                    let value =
                        u64::from_le_bytes(fixed[start + at..start + at + 8].try_into().unwrap());
                    sizes.push(format!("+{at:#x}={value}"));
                }
                out.push(format!("    sizes: {}", sizes.join(" ")));
                for (label, pairs_at) in [("a", 0x20usize), ("b", 0x1C)] {
                    if start + pairs_at + 2 > fixed.len() {
                        continue;
                    }
                    let pairs =
                        u16::from_le_bytes(fixed[start + pairs_at..start + pairs_at + 2].try_into().unwrap())
                            as usize;
                    let runs = if pairs <= attribute.length {
                        parse_runs(&fixed[start + pairs..start + attribute.length])
                    } else {
                        Vec::new()
                    };
                    out.push(format!(
                        "    layout {label}: pairs_at={pairs_at} pairs={pairs} runs={:?}",
                        runs
                    ));
                }
            }
            if attribute.kind == ATTR_FILE_NAME {
                let value = attribute.value;
                out.push(format!("    name value: {}", hex_of(&value[..value.len().min(0x50)])));
                out.push(format!(
                    "    namespace={} length_byte={} trailing={:?}",
                    attribute.namespace,
                    value.get(0x40).map(|byte| *byte as usize).unwrap_or(0),
                    value.get(0x41).map(|byte| *byte as usize).unwrap_or(0)
                ));
            }
        });
        out
    }

    /// The size of one record.
    pub fn record_size(&self) -> u32 {
        self.record_size
    }

    /// The raw bytes of one record, and what was made of them.
    ///
    /// This is what the probe shows when a record does not give up its name.
    pub fn dump(&self, number: u64) -> Vec<String> {
        let mut out = Vec::new();
        let Some(offset) = self.offset_of(number) else {
            out.push(format!("  record {number} is not on the disk"));
            return out;
        };
        let mut buffer = vec![0u8; self.record_size as usize];
        if !unsafe { read_at(self.handle, offset as i64, &mut buffer) } {
            out.push(format!(
                "  record {number} could not be read (error {})",
                unsafe { sys::GetLastError() }
            ));
            return out;
        }
        out.push(format!(
            "  record {number} at offset {offset}: {}",
            hex_of(&buffer[..64.min(buffer.len())])
        ));
        out.push(format!(
            "  signature={:?} usa_offset={} usa_count={} attributes_offset={} flags={:#x}",
            String::from_utf8_lossy(&buffer[0..4]),
            u16::from_le_bytes(buffer[0x04..0x06].try_into().unwrap()),
            u16::from_le_bytes(buffer[0x06..0x08].try_into().unwrap()),
            u16::from_le_bytes(buffer[0x14..0x16].try_into().unwrap()),
            u16::from_le_bytes(buffer[0x16..0x18].try_into().unwrap()),
        ));
        let mut fixed = buffer.clone();
        fix_up(&mut fixed);
        let attributes_offset = u16::from_le_bytes(fixed[0x14..0x16].try_into().unwrap()) as usize;
        walk_attributes(&fixed, attributes_offset, |attribute| {
            out.push(format!(
                "  attribute kind={:#x} non_resident={} name={:?} value={} data_size={:?} runs={}",
                attribute.kind,
                attribute.non_resident,
                attribute.name(),
                attribute.value.len(),
                attribute.data_size,
                attribute.runs.len()
            ));
            if attribute.kind == ATTR_FILE_NAME {
                let value = attribute.value;
                out.push(format!(
                    "    name value: {}",
                    hex_of(&value[..value.len().min(0x50)])
                ));
                out.push(format!(
                    "    namespace={} length_byte={}",
                    attribute.namespace,
                    value.get(0x40).map(|byte| *byte as usize).unwrap_or(0)
                ));
            }
        });
        out
    }

    /// Read one record of the table.
    pub fn record(&self, number: u64) -> Option<MftRecord> {
        let offset = self.offset_of(number)?;
        let mut buffer = vec![0u8; self.record_size as usize];
        if !unsafe { read_at(self.handle, offset as i64, &mut buffer) } {
            return None;
        }
        let mut scratch = String::new();
        let mut extensions: Vec<u64> = Vec::new();
        parse_record(&mut buffer, &mut scratch, &mut extensions, number).map(|fields| MftRecord {
            number: fields.number,
            sequence: fields.sequence,
            name: scratch,
            parent: fields.parent,
            size: fields.size,
            created: fields.created,
            modified: fields.modified,
            accessed: fields.accessed,
            is_directory: fields.is_directory,
        })
    }

    /// Where a record starts on the disk.
    fn offset_of(&self, number: u64) -> Option<u64> {
        let cluster_size = self.cluster_size();
        let logical = number * self.record_size as u64;
        let extent = self.extent_at(logical)?;
        let inside = logical - extent.logical;
        Some(extent.lcn * cluster_size + inside)
    }

    /// Walk over every record of the table.
    ///
    /// The table is read in pieces of a few megabytes and every record is
    /// handed over as soon as it has been read, so the memory does not grow
    /// with the size of the table. The name is written into a buffer that is
    /// reused for every record, so no memory is allocated per record either.
    ///
    /// This is how Everything reads the table as well: one pass, in order.
    pub fn for_each_record(&self, visit: impl FnMut(&str, RecordFields)) -> MftStats {
        self.for_each_record_with_progress(visit, None)
    }

    /// Walk over every record of the table, saying how much has been read.
    ///
    /// The second argument of the report is the number of bytes the table has
    /// altogether, so that the caller can show how far the work is.
    pub fn for_each_record_with_progress(
        &self,
        mut visit: impl FnMut(&str, RecordFields),
        progress: Option<&(dyn Fn(u64, u64) + Sync)>,
    ) -> MftStats {
        let mut stats = MftStats::default();
        let cluster_size = self.cluster_size();
        let record_size = self.record_size as u64;
        let total_bytes = self.total_clusters() * cluster_size;
        let chunk = chunk_bytes(cluster_size as usize);
        // The buffer for the read has to be aligned to the sector, so it is
        // not an ordinary vector.
        let mut buffer = match SectorBuffer::new(chunk) {
            Some(buffer) => buffer,
            None => return stats,
        };
        let mut scratch = String::new();
        let mut extensions: Vec<u64> = Vec::new();
        let mut logical = 0u64;
        let mut number = 0u64;
        let mut empty = 0u64;
        while logical + record_size <= total_bytes {
            let want = chunk.min((total_bytes - logical) as usize);
            let started = std::time::Instant::now();
            let read = self.read_bytes(logical, buffer.as_slice()[..want].as_mut());
            stats.read_ms += started.elapsed().as_millis() as u64;
            stats.bytes += read as u64;
            // The progress is reported for every piece that was read, so that
            // the bar moves while the disk is being read.
            if let Some(report) = progress {
                report(stats.bytes, total_bytes);
            }
            if read < record_size as usize {
                stats.error = unsafe { sys::GetLastError() };
                break;
            }
            // Only whole records are used; the rest of the piece is the
            // beginning of the next one.
            let usable = read - (read % record_size as usize);
            let started = std::time::Instant::now();
            for slot in 0..(usable / record_size as usize) {
                let start = slot * record_size as usize;
                let slice = &mut buffer.as_slice()[start..start + record_size as usize];
                let number = number + slot as u64;
                let parsed = parse_record(slice, &mut scratch, &mut extensions, number);
                let fields = match parsed {
                    Some(fields) => Some(fields),
                    // The attributes of a record do not always fit in it.
                    // The name is then in one of the other records, and the
                    // list in the record says which ones.
                    None if !extensions.is_empty() => {
                        stats.extensions += 1;
                        self.extension_fields(&extensions, &mut scratch)
                            .map(|mut fields| {
                                // The number is the one of the record that
                                // was read, not the one of the record it
                                // lives in.
                                fields.number = number;
                                fields
                            })
                    }
                    None => None,
                };
                match fields {
                    Some(fields) => {
                        empty = 0;
                        stats.records += 1;
                        visit(&scratch, fields);
                    }
                    None => {
                        // The table is allocated in pieces, so the part at the
                        // end that is not in use is empty. The count is only
                        // reported; the walk goes on, because a record without
                        // a name is not always an unused one.
                        empty += 1;
                    }
                }
            }
            stats.parse_ms += started.elapsed().as_millis() as u64;
            logical += usable as u64;
            number += (usable / record_size as usize) as u64;
        }
        stats.empty_tail = empty;
        stats
    }

    /// Read every record of the table into a list.
    ///
    /// The list is only used by the tests. The launcher walks the table with
    /// [`MftReader::for_each_record`] instead, because that way the memory
    /// does not grow with the size of the table.
    pub fn read_all(&self, out: &mut Vec<MftRecord>) {
        self.for_each_record(|name, fields| {
            out.push(MftRecord {
                number: fields.number,
                sequence: fields.sequence,
                name: name.to_string(),
                parent: fields.parent,
                size: fields.size,
                created: fields.created,
                modified: fields.modified,
                accessed: fields.accessed,
                is_directory: fields.is_directory,
            });
        });
    }

    /// The fields of an entry whose attributes are in other records.
    ///
    /// The list in the record says which records hold the rest of the
    /// attributes. The name is in one of them.
    fn extension_fields(&self, extensions: &[u64], scratch: &mut String) -> Option<RecordFields> {
        let mut buffer = vec![0u8; self.record_size as usize];
        let mut more: Vec<u64> = Vec::new();
        for reference in extensions {
            let Some(offset) = self.offset_of(*reference) else {
                continue;
            };
            if !unsafe { read_at(self.handle, offset as i64, &mut buffer) } {
                continue;
            }
            if let Some(fields) = parse_record(&mut buffer, scratch, &mut more, *reference) {
                return Some(fields);
            }
        }
        None
    }

    /// The size of one cluster in bytes.
    fn cluster_size(&self) -> u64 {
        (self.sectors_per_cluster * self.bytes_per_sector) as u64
    }

    /// How many clusters the table holds.
    fn total_clusters(&self) -> u64 {
        self.extents.iter().map(|extent| extent.clusters).sum()
    }

    /// The run of clusters that holds the given place in the table.
    fn extent_at(&self, logical: u64) -> Option<&Extent> {
        let cluster_size = self.cluster_size();
        self.extents.iter().find(|extent| {
            logical >= extent.logical
                && logical < extent.logical + extent.clusters * cluster_size
        })
    }

    /// Read a part of the table into the buffer.
    ///
    /// The table is not necessarily in one piece on the disk, so the read is
    /// split up at the places where the table jumps. The number of bytes that
    /// were read is returned; it is smaller than the size of the buffer when
    /// the end of the volume was reached.
    fn read_bytes(&self, logical: u64, buffer: &mut [u8]) -> usize {
        let cluster_size = self.cluster_size();
        let mut done = 0usize;
        let mut at = logical;
        while done < buffer.len() {
            let Some(extent) = self.extent_at(at) else {
                break;
            };
            let inside = at - extent.logical;
            let remaining = extent.clusters * cluster_size - inside;
            let take = remaining.min((buffer.len() - done) as u64) as usize;
            let physical = extent.lcn * cluster_size + inside;
            let read = unsafe {
                read_at_partial(self.raw_handle, physical as i64, &mut buffer[done..done + take])
            };
            done += read;
            if read < take {
                // The end of the volume was reached.
                break;
            }
            at += take as u64;
        }
        done
    }
}

/// A buffer for a read that goes straight to the disk.
///
/// Such a read has to be aligned to the size of a sector, and the ordinary
/// allocator does not promise that.
struct SectorBuffer {
    ptr: *mut u8,
    len: usize,
}

impl SectorBuffer {
    /// A buffer of the given size, aligned for a direct read.
    fn new(len: usize) -> Option<Self> {
        const ALIGN: usize = 4096;
        let layout = std::alloc::Layout::from_size_align(len, ALIGN).ok()?;
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return None;
        }
        Some(Self { ptr, len })
    }

    /// The buffer as a slice.
    fn as_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for SectorBuffer {
    fn drop(&mut self) {
        const ALIGN: usize = 4096;
        if let Ok(layout) = std::alloc::Layout::from_size_align(self.len, ALIGN) {
            unsafe { std::alloc::dealloc(self.ptr, layout) };
        }
    }
}

/// Read at an offset of the raw volume.
unsafe fn read_at(handle: sys::Handle, offset: i64, buffer: &mut [u8]) -> bool {
    unsafe { read_at_partial(handle, offset, buffer) == buffer.len() }
}

/// Read at an offset of the raw volume, as far as the disk goes.
///
/// The number of bytes that were read is returned. A short read is not an
/// error: it is what happens at the end of the volume.
unsafe fn read_at_partial(handle: sys::Handle, offset: i64, buffer: &mut [u8]) -> usize {
    let mut position = 0i64;
    if unsafe { sys::SetFilePointerEx(handle, offset, &mut position, 0) } == 0 {
        return 0;
    }
    let mut read = 0u32;
    let ok = unsafe {
        sys::ReadFile(
            handle,
            buffer.as_mut_ptr().cast(),
            buffer.len() as sys::Dword,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return 0;
    }
    read as usize
}

/// The size of one piece of the table that is read at a time.
///
/// A few megabytes keep the disk busy without taking much memory.
fn chunk_bytes(cluster_size: usize) -> usize {
    const TARGET: usize = 4 * 1024 * 1024;
    (TARGET / cluster_size).max(1) * cluster_size
}

/// Turn a string into a NUL terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Write bytes as a row of hex numbers.
fn hex_of(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<String>>()
        .join(" ")
}

/// Parse one record of the table.
///
/// The record is changed in place: the fixup puts the last two bytes of every
/// sector back. The name is written into `scratch`, which the caller reuses
/// for every record, so that nothing is allocated per record.
fn parse_record(
    buffer: &mut [u8],
    scratch: &mut String,
    extensions: &mut Vec<u64>,
    number: u64,
) -> Option<RecordFields> {
    if buffer.len() < 0x30 || &buffer[0..4] != b"FILE" {
        return None;
    }
    let sequence = u16::from_le_bytes(buffer[0x10..0x12].try_into().unwrap());
    let attributes_offset = u16::from_le_bytes(buffer[0x14..0x16].try_into().unwrap()) as usize;
    let flags = u16::from_le_bytes(buffer[0x16..0x18].try_into().unwrap());

    // The fixup: the last two bytes of every sector are replaced by the
    // update sequence array, and have to be put back.
    fix_up(buffer);

    let mut fields = RecordFields {
        number,
        sequence,
        is_directory: flags & 0x02 != 0,
        ..RecordFields::default()
    };
    let mut named = false;
    let mut has_standard = false;
    // How good the name that was taken is. A record can hold more than one
    // name, one per namespace, and only the best one is wanted.
    let mut best_rank = 0u8;
    // The list is emptied first: a record that has no list of its own must
    // not inherit the one of the record before it.
    extensions.clear();

    walk_attributes(buffer, attributes_offset, |attribute| {
        match attribute.kind {
            ATTR_STANDARD_INFORMATION => {
                // The times of the entry. The name attribute holds the same
                // ones, but this is the attribute the system keeps up to
                // date, so it is the one that is used.
                let value = attribute.value;
                if value.len() >= 0x20 {
                    fields.created = filetime_to_unix(&value[0x00..0x08]);
                    fields.modified = filetime_to_unix(&value[0x08..0x10]);
                    fields.accessed = filetime_to_unix(&value[0x18..0x20]);
                    has_standard = true;
                }
            }
            ATTR_FILE_NAME => {
                // A record can hold a name per namespace. The Win32 one is
                // the name Windows shows; the "Win32 and DOS" one is the same
                // name and is used by the files of the system; the POSIX one
                // is used by the folders that were made through the POSIX
                // part of the system; the DOS one is the short name and is
                // only a last resort.
                let rank = namespace_rank(attribute.namespace);
                // The first name of the best kind wins: a file with several
                // hard links has one name for each of them.
                if rank == 0 || rank <= best_rank {
                    return;
                }
                let value = attribute.value;
                if value.len() < 0x42 {
                    return;
                }
                // The parent is the number of the record of the folder, not
                // the whole file reference number: the sequence number of the
                // folder is of no use for the tree.
                fields.parent = u64::from_le_bytes(value[0x00..0x08].try_into().unwrap())
                    & 0x0000_FFFF_FFFF_FFFF;
                if !has_standard {
                    fields.created = filetime_to_unix(&value[0x08..0x10]);
                    fields.modified = filetime_to_unix(&value[0x10..0x18]);
                    fields.accessed = filetime_to_unix(&value[0x18..0x20]);
                }
                // The real size of the file lives here as well.
                let real_size = u64::from_le_bytes(value[0x30..0x38].try_into().unwrap());
                if real_size > 0 {
                    fields.size = real_size;
                }
                let name_length = value[0x40] as usize;
                if value.len() >= 0x42 + name_length * 2 {
                    scratch.clear();
                    push_utf16(scratch, &value[0x42..0x42 + name_length * 2]);
                    named = true;
                    best_rank = rank;
                }
            }
            ATTR_DATA => {
                // A resident attribute holds the content itself; a non
                // resident one has the size in its header.
                if !attribute.non_resident {
                    if attribute.value.len() as u64 > fields.size {
                        fields.size = attribute.value.len() as u64;
                    }
                } else if let Some(length) = attribute.data_size {
                    if length > fields.size {
                        fields.size = length;
                    }
                }
            }
            ATTR_ATTRIBUTE_LIST => {
                // The attributes of this record do not all fit in it. The
                // list says which records hold the rest of them, and the
                // name is often one of those.
                fields.split = true;
                extension_records(attribute.value, extensions);
            }
            _ => {}
        }
    });

    if !named {
        return None;
    }
    Some(fields)
}

/// The extents of the unnamed content attribute of a record.
///
/// Only the record of the table itself needs them: the content of that
/// record is the table.
fn data_runs_of(buffer: &[u8], attributes_offset: usize) -> Vec<(u64, u64)> {
    let mut runs = Vec::new();
    walk_attributes(buffer, attributes_offset, |attribute| {
        if attribute.kind == ATTR_DATA && runs.is_empty() && attribute.name_len == 0 {
            runs = parse_runs(attribute.runs);
        }
    });
    runs
}

/// How good the name of a namespace is.
///
/// A record can hold a name per namespace. The Win32 name is the one
/// Windows shows; the "Win32 and DOS" name is the same name and is used by
/// the files of the system; the POSIX name is used by the folders that were
/// made through the POSIX part of the system; the DOS name is the short
/// name and is only a last resort.
fn namespace_rank(namespace: u8) -> u8 {
    match namespace {
        1 => 4,
        3 => 3,
        0 => 2,
        2 => 1,
        _ => 0,
    }
}

/// The records that hold the attributes a record could not hold itself.
///
/// The list is a run of entries, each of which names the kind of the
/// attribute, the record that holds it and the place in the file where that
/// part starts.
fn extension_records(list: &[u8], out: &mut Vec<u64>) {
    let mut at = 0usize;
    while at + 26 <= list.len() {
        let kind = u32::from_le_bytes(list[at..at + 4].try_into().unwrap());
        if kind == ATTR_END {
            break;
        }
        let length = u16::from_le_bytes(list[at + 4..at + 6].try_into().unwrap()) as usize;
        if length < 26 || at + length > list.len() {
            break;
        }
        // The file reference number of the record that holds the attribute.
        let reference =
            u64::from_le_bytes(list[at + 0x10..at + 0x18].try_into().unwrap())
                & 0x0000_FFFF_FFFF_FFFF;
        if reference != 0 {
            out.push(reference);
        }
        at += length;
    }
}

/// Put the UTF-16 text of a name into a string.
///
/// The text of a name is not always valid: a lone surrogate stands for a
/// character that cannot be shown, and it is replaced the way Windows does it.
fn push_utf16(out: &mut String, bytes: &[u8]) {
    // The names on a real volume are plain ASCII almost every time, and then
    // the text is simply every second byte. That case is worth a pass of its
    // own: it is the common one and it is four times cheaper.
    if bytes.len() % 2 == 0 && bytes.iter().skip(1).step_by(2).all(|byte| *byte == 0) {
        for at in (0..bytes.len()).step_by(2) {
            out.push(bytes[at] as char);
        }
        return;
    }
    let mut at = 0usize;
    while at + 1 < bytes.len() {
        let unit = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        at += 2;
        if (0xD800..0xDC00).contains(&unit) {
            // The first half of a pair: the second half follows.
            if at + 1 < bytes.len() {
                let low = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
                if (0xDC00..0xE000).contains(&low) {
                    at += 2;
                    let code =
                        0x1_0000 + (((unit as u32 - 0xD800) << 10) | (low as u32 - 0xDC00));
                    out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    continue;
                }
            }
            out.push('\u{FFFD}');
            continue;
        }
        if (0xDC00..0xE000).contains(&unit) {
            out.push('\u{FFFD}');
            continue;
        }
        out.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
    }
}

/// Put the original bytes back at the end of every sector.
///
/// The update sequence array holds the sequence number first and then one
/// value per sector: the value of the first sector is at index one, the
/// value of the second at index two, and so on.
fn fix_up(buffer: &mut [u8]) {
    if buffer.len() < 0x08 {
        return;
    }
    let usa_offset = u16::from_le_bytes(buffer[0x04..0x06].try_into().unwrap()) as usize;
    let usa_count = u16::from_le_bytes(buffer[0x06..0x08].try_into().unwrap()) as usize;
    if usa_count < 2 || usa_offset + usa_count * 2 > buffer.len() {
        return;
    }
    for index in 1..usa_count {
        let value = u16::from_le_bytes(
            buffer[usa_offset + index * 2..usa_offset + (index + 1) * 2]
                .try_into()
                .unwrap(),
        );
        let position = index * 512 - 2;
        if position + 2 <= buffer.len() {
            buffer[position..position + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
}

/// One attribute of a record.
struct Attribute<'a> {
    /// The record the attribute belongs to.
    record: &'a [u8],
    /// Where the attribute starts in the record.
    start: usize,
    /// How long the attribute is.
    length: usize,
    kind: u32,
    /// True when the content lives outside the record.
    non_resident: bool,
    /// The value of a resident attribute.
    value: &'a [u8],
    /// The size of the content of a non resident attribute.
    data_size: Option<u64>,
    /// How many characters the name of the attribute has.
    name_len: u8,
    /// Where the name of the attribute is in the record.
    name_offset: usize,
    /// The namespace of the name of a file name attribute.
    namespace: u8,
    /// The list of extents of a non resident attribute.
    runs: &'a [u8],
}

impl<'a> Attribute<'a> {
    /// The name of the attribute.
    ///
    /// Only the diagnostics need it, so it is turned into text on demand.
    fn name(&self) -> String {
        if self.name_len == 0 {
            return String::new();
        }
        let from = self.start + self.name_offset;
        let to = from + self.name_len as usize * 2;
        if to > self.record.len() {
            return String::new();
        }
        let mut out = String::new();
        push_utf16(&mut out, &self.record[from..to]);
        out
    }
}

/// Walk the attributes of a record.
///
/// The attributes are handed over one by one, so that walking over the
/// millions of records of a table does not allocate anything.
fn walk_attributes<'a>(buffer: &'a [u8], start: usize, mut visit: impl FnMut(Attribute<'a>)) {
    let mut at = start;
    while at + 16 <= buffer.len() {
        let kind = u32::from_le_bytes(buffer[at..at + 4].try_into().unwrap());
        if kind == ATTR_END {
            break;
        }
        let length = u32::from_le_bytes(buffer[at + 4..at + 8].try_into().unwrap()) as usize;
        if length < 16 || at + length > buffer.len() {
            break;
        }
        let name_length = buffer[at + 9] as usize;
        let name_offset = u16::from_le_bytes(buffer[at + 10..at + 12].try_into().unwrap()) as usize;
        // The byte says whether the content lives outside the record.
        let non_resident = buffer[at + 8] != 0;

        let mut value: &[u8] = &[];
        let mut data_size = None;
        let mut runs: &[u8] = &[];
        if !non_resident {
            // The header of the content is eight bytes long, and it is only
            // there when the attribute is long enough for it. A record that
            // says otherwise is skipped — reading past the end of the buffer
            // would take the whole program down with it.
            if at + 24 <= buffer.len() {
                let value_length =
                    u32::from_le_bytes(buffer[at + 16..at + 20].try_into().unwrap()) as usize;
                let value_offset =
                    u16::from_le_bytes(buffer[at + 20..at + 22].try_into().unwrap()) as usize;
                if value_offset <= length
                    && at
                        .checked_add(value_offset)
                        .and_then(|start| start.checked_add(value_length))
                        .is_some_and(|end| end <= buffer.len())
                {
                    value = &buffer[at + value_offset..at + value_offset + value_length];
                }
            }
        } else {
            // The header of a content attribute that lives outside the
            // record:
            //
            // | place | meaning |
            // |---|---|
            // | `+0x10` | the first cluster of the content |
            // | `+0x18` | the last cluster of the content |
            // | `+0x20` | where the list of the extents starts |
            // | `+0x28` | the space the content takes on the disk |
            // | `+0x30` | the size of the content |
            // | `+0x38` | the part of the content that is written |
            //
            // The places were read out of the raw bytes of a real table on
            // this machine: the size at `+0x2C` is the tail of the space and
            // the head of the size, so it is a number that means nothing.
            if at + 0x38 <= buffer.len() {
                data_size = Some(
                    u64::from_le_bytes(buffer[at + 0x30..at + 0x38].try_into().unwrap()),
                );
            }
            // The list of extents starts at the offset the header names.
            if at + 0x22 <= buffer.len() {
                let mapping_pairs_offset =
                    u16::from_le_bytes(buffer[at + 0x20..at + 0x22].try_into().unwrap()) as usize;
                if mapping_pairs_offset <= length {
                    runs = &buffer[at + mapping_pairs_offset..at + length];
                }
            }
        }

        // The namespace of a file name attribute sits behind its name.
        let mut namespace = 0u8;
        if kind == ATTR_FILE_NAME && value.len() > 0x41 {
            namespace = value[0x41];
        }

        visit(Attribute {
            record: buffer,
            start: at,
            length,
            kind,
            non_resident,
            value,
            data_size,
            name_len: name_length as u8,
            name_offset,
            namespace,
            runs,
        });
        at += length;
    }
}

/// Read the list of extents of a non resident attribute.
///
/// The list is stored as "mapping pairs": a byte that says how many bytes the
/// length and the offset take, then the length of the run and the distance to
/// the next cluster.
fn parse_runs(runs: &[u8]) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut cluster = 0i64;
    while at < runs.len() {
        let header = runs[at];
        if header == 0 {
            break;
        }
        at += 1;
        let length_size = (header & 0x0F) as usize;
        let offset_size = ((header >> 4) & 0x0F) as usize;
        if length_size == 0 || at + length_size + offset_size > runs.len() {
            break;
        }
        let mut length = 0u64;
        for index in 0..length_size {
            length |= (runs[at + index] as u64) << (8 * index);
        }
        at += length_size;
        if offset_size == 0 {
            // A run without a cluster number is a hole in the file.
            continue;
        }
        let mut delta = 0i64;
        for index in 0..offset_size {
            delta |= (runs[at + index] as i64) << (8 * index);
        }
        // The distance is a signed number, so the top bit has to be extended.
        // A distance that is eight bytes long is already a whole number, and
        // extending it would turn the sign of a positive distance into a
        // negative one — which sends the next run backwards.
        if offset_size < 8 && runs[at + offset_size - 1] & 0x80 != 0 {
            delta |= !0i64 << (8 * offset_size);
        }
        at += offset_size;
        cluster += delta;
        out.push((cluster as u64, length));
    }
    out
}

/// Convert a FILETIME into seconds since the Unix epoch.
fn filetime_to_unix(raw: &[u8]) -> u64 {
    let ticks = u64::from_le_bytes(raw.try_into().unwrap_or([0; 8]));
    ticks.saturating_sub(116_444_736_000_000_000) / 10_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a record the way the driver would lay it out.
    ///
    /// The layout is the one of a real table entry: the `FILE` signature, the
    /// fixup array, the two attributes (`$DATA` with its list of extents and
    /// `$FILE_NAME` with the name and the times).
    fn build_record(name: &str, parent: u64, size: u64, runs: &[u8]) -> Vec<u8> {        let mut record = vec![0u8; 1024];

        // ---- the header ----------------------------------------------------
        record[0..4].copy_from_slice(b"FILE");
        record[0x04..0x06].copy_from_slice(&0x28u16.to_le_bytes()); // usa offset
        record[0x06..0x08].copy_from_slice(&3u16.to_le_bytes()); // usa count
        record[0x10..0x12].copy_from_slice(&7u16.to_le_bytes()); // sequence
        record[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // attributes
        record[0x16..0x18].copy_from_slice(&2u16.to_le_bytes()); // a folder
        record[0x18..0x1C].copy_from_slice(&1024u32.to_le_bytes()); // bytes in use

        // ---- the $DATA attribute -------------------------------------------
        // The length is rounded up to a multiple of eight, the way the
        // driver writes it.
        let data_length = (0x40 + runs.len() + 7) & !7;
        let mut at = 0x38usize;
        record[at..at + 4].copy_from_slice(&ATTR_DATA.to_le_bytes()); // $DATA
        record[at + 4..at + 8].copy_from_slice(&(data_length as u32).to_le_bytes());
        record[at + 8] = 1; // not resident
        record[at + 9] = 0; // no name
        record[at + 10..at + 12].copy_from_slice(&0x40u16.to_le_bytes());
        record[at + 0x10..at + 0x18].copy_from_slice(&0u64.to_le_bytes()); // lowest vcn
        record[at + 0x18..at + 0x20].copy_from_slice(&0x10u64.to_le_bytes()); // highest vcn
        record[at + 0x20..at + 0x22].copy_from_slice(&0x40u16.to_le_bytes()); // mapping pairs
        record[at + 0x24..at + 0x2C].copy_from_slice(&0x1000u64.to_le_bytes()); // reserved
        record[at + 0x28..at + 0x30].copy_from_slice(&0x1000u64.to_le_bytes()); // allocated
        record[at + 0x30..at + 0x38].copy_from_slice(&size.to_le_bytes()); // file size
        record[at + 0x38..at + 0x40].copy_from_slice(&size.to_le_bytes()); // initialised
        record[at + 0x40..at + 0x40 + runs.len()].copy_from_slice(runs);
        at += data_length;

        // ---- the $FILE_NAME attribute ---------------------------------------
        let name_units: Vec<u16> = name.encode_utf16().collect();
        let value_length = 0x42 + name_units.len() * 2;
        let name_length = (0x18 + value_length + 7) & !7;
        record[at..at + 4].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes()); // $FILE_NAME
        record[at + 4..at + 8].copy_from_slice(&(name_length as u32).to_le_bytes());
        record[at + 8] = 0; // resident
        record[at + 9] = 0; // no name
        record[at + 10..at + 12].copy_from_slice(&0x42u16.to_le_bytes());
        record[at + 16..at + 20].copy_from_slice(&(value_length as u32).to_le_bytes());
        record[at + 20..at + 22].copy_from_slice(&0x18u16.to_le_bytes());
        let value = at + 0x18;
        record[value..value + 8].copy_from_slice(&parent.to_le_bytes());
        record[value + 0x28..value + 0x30].copy_from_slice(&size.to_le_bytes());
        record[value + 0x40] = name_units.len() as u8;
        record[value + 0x41] = 1; // the Win32 namespace
        for (index, unit) in name_units.iter().enumerate() {
            let at = value + 0x42 + index * 2;
            record[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }
        at += name_length as usize;

        // ---- the end of the record -----------------------------------------
        record[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        record[at + 4..at + 8].copy_from_slice(&8u32.to_le_bytes());

        // ---- the fixup -----------------------------------------------------
        // The last two bytes of every sector are replaced by the update
        // sequence array.
        record[510] = 0xAA;
        record[511] = 0xBB;
        record[1022] = 0xCC;
        record[1023] = 0xDD;
        record[0x28..0x2A].copy_from_slice(&1u16.to_le_bytes()); // the sequence
        record[0x2A..0x2C].copy_from_slice(&0xBBAAu16.to_le_bytes());
        record[0x2C..0x2E].copy_from_slice(&0xDDCCu16.to_le_bytes());
        record[510..512].copy_from_slice(&0xBBAAu16.to_le_bytes());
        record[1022..1024].copy_from_slice(&0xDDCCu16.to_le_bytes());
        record
    }

    #[test]
    fn a_record_is_read_field_by_field() {
        // One run of eight clusters at cluster 0x20.
        let runs = [0x11, 0x08, 0x20];
        let mut record = build_record("report.pdf", 42, 4096, &runs);
        let mut scratch = String::new();
        let fields = parse_record(&mut record, &mut scratch, &mut Vec::new(), 7).expect("the record should be read");
        assert_eq!(scratch, "report.pdf");
        assert_eq!(fields.parent, 42);
        assert_eq!(fields.size, 4096);
        assert!(fields.is_directory);
        assert_eq!(fields.sequence, 7);
        assert_eq!((fields.sequence as u64) << 48 | 7, 7u64 << 48 | 7);
    }

    #[test]
    fn the_extents_are_read() {
        let runs = [0x11, 0x08, 0x20];
        let mut record = build_record("$MFT", 5, 0x1000, &runs);
        fix_up(&mut record);
        let attributes_offset =
            u16::from_le_bytes(record[0x14..0x16].try_into().unwrap()) as usize;
        assert_eq!(data_runs_of(&record, attributes_offset), vec![(0x20, 8)]);
    }

    #[test]
    fn the_fixup_puts_the_bytes_back() {
        let runs = [0x11, 0x08, 0x20];
        let record = build_record("report.pdf", 42, 4096, &runs);
        // The fixup has to restore the last two bytes of every sector.
        let mut fixed = record.clone();
        fix_up(&mut fixed);
        assert_eq!(&fixed[510..512], &[0xAA, 0xBB]);
        assert_eq!(&fixed[1022..1024], &[0xCC, 0xDD]);
        // The name is somewhere after the first sector, so it is untouched.
        let mut record = record.clone();
        let mut scratch = String::new();
        parse_record(&mut record, &mut scratch, &mut Vec::new(), 1).expect("the record should be read");
        assert_eq!(scratch, "report.pdf");
    }

    #[test]
    fn the_name_of_another_namespace_is_taken_when_there_is_none() {
        // The files of the system use the "Win32 and DOS" namespace.
        let mut record = build_record("$MFT", 5, 0x1000, &[0x11, 0x08, 0x20]);
        let value = find_name_value(&mut record);
        value[0x41] = 3;
        let mut scratch = String::new();
        let fields = parse_record(&mut record, &mut scratch, &mut Vec::new(), 0).expect("the record should be read");
        assert_eq!(scratch, "$MFT");
        assert_eq!(fields.parent, 5);
    }

    #[test]
    fn a_name_in_utf16_is_turned_into_text() {
        let mut out = String::new();
        // "中文" followed by a pair that stands for an emoji.
        let units: [u16; 4] = [0x4E2D, 0x6587, 0xD83D, 0xDE00];
        let bytes: Vec<u8> = units
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        push_utf16(&mut out, &bytes);
        assert_eq!(out, "中文😀");
    }

    #[test]
    fn a_lone_surrogate_is_replaced() {
        let mut out = String::new();
        push_utf16(&mut out, &[0x00, 0xD8]);
        assert_eq!(out, "\u{FFFD}");
    }

    /// The value of the name attribute of a record.
    fn find_name_value(record: &mut [u8]) -> &mut [u8] {
        let attributes_offset =
            u16::from_le_bytes(record[0x14..0x16].try_into().unwrap()) as usize;
        let mut found = None;
        walk_attributes(record, attributes_offset, |attribute| {
            if attribute.kind == ATTR_FILE_NAME && found.is_none() {
                let start = attribute.value.as_ptr() as usize - attribute.record.as_ptr() as usize;
                found = Some(start);
            }
        });
        let start = found.expect("the record should have a name attribute");
        let length = {
            let mut length = 0usize;
            walk_attributes(record, attributes_offset, |attribute| {
                if attribute.kind == ATTR_FILE_NAME {
                    length = attribute.value.len();
                }
            });
            length
        };
        &mut record[start..start + length]
    }

    #[test]
    fn the_runs_are_read() {
        // One run of 8 clusters at cluster 4.
        let runs = [0x11, 0x08, 0x04];
        assert_eq!(parse_runs(&runs), vec![(4, 8)]);
    }

    #[test]
    fn several_runs_follow_each_other() {
        // Two runs: 4 clusters at 0x20, then 2 clusters 0x10 further on.
        let runs = [0x11, 0x04, 0x20, 0x11, 0x02, 0x10];
        assert_eq!(parse_runs(&runs), vec![(0x20, 4), (0x30, 2)]);
    }

    #[test]
    fn a_backwards_run_is_signed() {
        // One cluster at 0, then one cluster 0x80 further back.
        let runs = [0x11, 0x01, 0x00, 0x11, 0x01, 0x80];
        assert_eq!(parse_runs(&runs), vec![(0, 1), (0xFFFF_FFFF_FFFF_FF80, 1)]);
    }

    #[test]
    fn the_file_time_is_converted() {
        // The moment the Unix epoch began.
        let raw = 116_444_736_000_000_000u64.to_le_bytes();
        assert_eq!(filetime_to_unix(&raw), 0);
    }
}
