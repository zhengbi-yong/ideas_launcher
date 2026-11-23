use super::*;

/// The index while it is being filled.
///
/// The entries are pushed in one at a time, so the flat list of them never
/// exists on its own: the memory stays at the size of the index. On a volume
/// with millions of files that is the difference between a peak of twice the
/// size of the index and a peak of the size of the index itself.

pub struct EntrySink {
    pub(in crate::fsindex) index: FsIndex,
    /// Where the index of the entry of a record is kept.
    pub(in crate::fsindex) lookup: Lookup,
    /// The entries whose folder is not known yet.
    ///
    /// A folder can have a higher number than the files in it, so it is
    /// sometimes read later than the files that belong to it.
    pending: Vec<(u32, u64)>,
    /// The volume that is being filled.
    volume: u32,
    /// Where the entries of the volume that is being filled start.
    volume_start: u32,
    /// How many entries the volume holds, when that is known.
    ///
    /// The tables are made the right size the first time, so that they do
    /// not have to grow while the entries come in.
    expected: usize,
}

/// Where the index of the entry of a record is kept.
pub(super) enum Lookup {
    /// A plain array with one place per record of the table.
    ///
    /// The table of a volume is read in order, so the numbers of the records
    /// grow and the place of a record is its number. This is the quickest way
    /// to find the folder of an entry and it costs four bytes per record of
    /// the table.
    Dense { table: Vec<u32> },
    /// A map, for the entries that do not come from a table.
    ///
    /// The change journal hands out the entries in the order of the changes,
    /// and the numbers of the records are all over the place, so a plain
    /// array would be mostly empty.
    Sparse(FrnMap),
}

impl Lookup {
    /// Remember the index of the entry of a record.
    pub(super) fn insert(&mut self, number: u64, index: u32) {
        match self {
            Lookup::Dense { table } => {
                let number = number as usize;
                if number >= table.len() {
                    table.resize(number + 1, NO_PARENT);
                }
                table[number] = index;
            }
            Lookup::Sparse(map) => map.insert(number, index),
        }
    }

    /// The index of the entry of a record.
    pub(super) fn lookup(&self, number: u64) -> Option<u32> {
        match self {
            Lookup::Dense { table } => {
                let number = number as usize;
                table
                    .get(number)
                    .copied()
                    .filter(|&index| index != NO_PARENT)
            }
            Lookup::Sparse(map) => map.lookup(number).copied(),
        }
    }
}

impl EntrySink {
    /// An empty index for the given volumes.
    pub(super) fn new(volumes: &[usn::Volume]) -> Self {
        let mut index = FsIndex::default();
        for volume in volumes {
            index.volumes.push(IndexedVolume {
                root: volume.root.clone(),
                device: volume.device.clone(),
                label: volume.label.clone(),
            });
        }
        Self {
            index,
            lookup: Lookup::Sparse(FrnMap::with_capacity(1024)),
            pending: Vec::new(),
            volume: 0,
            volume_start: 0,
            expected: 0,
        }
    }

    /// Say how many entries the volume holds, when that is known.
    ///
    /// The tables are made the right size the first time, so that they do
    /// not have to grow while the entries come in.
    pub fn reserve(&mut self, entries: usize) {
        self.expected = entries;
        self.index.entries.reserve(entries);
        self.index.masks.reserve(entries);
        self.index.path_masks.reserve(entries);
        self.index.frns.reserve(entries);
        self.index.meta_known.reserve(entries);
        self.index.excluded.reserve(entries);
    }

    /// Say where the change journal of a volume stood when it was read.
    ///
    /// The number belongs to the volume of that number, and it is what makes
    /// the difference between reading the whole disk again and reading the
    /// few hundred kilobytes of the change journal.
    pub fn set_journal_point(&mut self, volume: u32, point: u64) {
        let points = &mut self.index.journal_points;
        if points.len() <= volume as usize {
            points.resize(volume as usize + 1, 0);
        }
        points[volume as usize] = point;
    }

    /// Start the entries of a volume.
    ///
    /// The `dense` flag says whether the entries come from a table, where the
    /// numbers of the records grow and a plain array can be used.
    pub fn begin_volume(&mut self, volume: u32, dense: bool) {
        self.volume = volume;
        self.volume_start = self.index.entries.len() as u32;
        self.pending.clear();
        self.lookup = if dense {
            Lookup::Dense {
                table: Vec::new(),
            }
        } else {
            Lookup::Sparse(FrnMap::with_capacity(self.expected.max(1024)))
        };
    }

    /// Add one entry, with the name that the table gives it.
    pub fn add(&mut self, name: &str, fields: &usn::RecordFields) {
        let name_start = self.index.names.len() as u32;
        self.index.names.push_str(name);
        self.push(usn::RawEntry {
            frn: (fields.sequence as u64) << 48 | (fields.number & 0x0000_FFFF_FFFF_FFFF),
            parent_frn: fields.parent,
            name_start,
            name_len: name.len() as u32,
            attributes: if fields.is_directory {
                usn::FILE_ATTRIBUTE_DIRECTORY
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
    }

    /// Add one entry that was read somewhere else.
    ///
    /// The name is put in the arena of the index, so the place of the name in
    /// the entry is worked out here.
    pub fn add_named(&mut self, name: &str, entry: &usn::RawEntry) {
        let name_start = self.index.names.len() as u32;
        self.index.names.push_str(name);
        let mut entry = *entry;
        entry.name_start = name_start;
        entry.name_len = name.len() as u32;
        self.push(entry);
    }

    /// Add one entry that was read somewhere else.
    pub fn add_raw(&mut self, entry: &usn::RawEntry) {
        self.push(*entry);
    }

    /// Add the entry to the tree.
    pub(super) fn push(&mut self, entry: usn::RawEntry) {
        // The root of a volume has an empty name; it is the anchor of the
        // tree rather than an entry of its own.
        if entry.name_len == 0 {
            return;
        }
        // The number of the record is the lower half of the file reference
        // number; the sequence number is of no use for the tree.
        let number = entry.frn & 0x0000_FFFF_FFFF_FFFF;
        let index = self.index.entries.len() as u32;
        self.lookup.insert(number, index);
        self.index.push_entry(self.volume, &entry, NO_PARENT);

        // The folder of the entry, when it is already known.
        let parent = entry.parent_frn & 0x0000_FFFF_FFFF_FFFF;
        if parent == number {
            return;
        }
        match self.lookup.lookup(parent) {
            Some(folder) => self.index.entries[index as usize].parent = folder,
            None => self.pending.push((index, parent)),
        }
    }

    /// Finish the entries of a volume.
    ///
    /// The folders that were not known yet are looked up now that the whole
    /// table has been read.
    pub fn end_volume(&mut self) {
        for (index, parent) in self.pending.drain(..) {
            let folder = match self.lookup.lookup(parent) {
                Some(folder) if folder != index => folder,
                _ => NO_PARENT,
            };
            self.index.entries[index as usize].parent = folder;
        }
        self.index.volume_ranges.push(VolumeRange {
            start: self.volume_start,
            end: self.index.entries.len() as u32,
            volume: self.volume,
        });
    }

    /// The finished index.
    pub fn finish(mut self, exclusions: &Exclusions) -> FsIndex {
        // Every volume has a place in the change journal, even when that place
        // is not known — then it is zero, and the volume has to be read again.
        let count = self.index.volumes.len();
        if self.index.journal_points.len() < count {
            self.index.journal_points.resize(count, 0);
        }
        self.index.build_pinyin();
        self.index.build_depths();
        self.index.build_masks();
        self.index.apply_exclusions(exclusions);
        // The table was read in the order of the numbers, so they are in
        // order and a file can be found by a binary search.
        self.index.frns_in_order = self.index.frns_are_in_order();
        self.index
    }
}
