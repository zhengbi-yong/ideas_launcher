use super::*;

impl FsIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many entries the index holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the index holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The volumes that are covered.
    pub fn volumes(&self) -> &[IndexedVolume] {
        &self.volumes
    }

    /// How many bytes the names of the entries take.
    ///
    /// All the names live in one string, so this is the whole of it.
    pub fn name_bytes(&self) -> usize {
        self.names.len()
    }

    /// Give the memory that the growth of the tables left over back.
    ///
    /// A table that grows by doubling holds twice the memory it needs when
    /// it is half full, and an index of a million entries holds a few dozen
    /// megabytes more than it needs. The walk is over at this point, so the
    /// tables are squeezed once and then left alone.
    pub fn shrink(&mut self) {
        self.names.shrink_to_fit();
        self.entries.shrink_to_fit();
        self.masks.shrink_to_fit();
        self.path_masks.shrink_to_fit();
        self.excluded.shrink_to_fit();
        self.meta_known.shrink_to_fit();
        self.frns.shrink_to_fit();
    }

    /// How many bytes the entries take, apart from their names.
    pub fn entry_bytes(&self) -> usize {
        self.entries.len() * std::mem::size_of::<FsEntry>()
            + self.masks.len() * 8
            + self.path_masks.len() * 8
            + self.excluded.len()
            + self.meta_known.len()
            + self.frns.len() * 8
    }

    /// The name of one entry.
    pub fn name(&self, index: u32) -> &str {
        let entry = &self.entries[index as usize];
        &self.names[entry.name_start as usize..(entry.name_start + entry.name_len as u32) as usize]
    }

    /// Build the index from the entries of the given volumes.
    ///
    /// The input is the flat list the MFT enumeration produces: every entry
    /// knows its own name and the file reference number of its folder. The
    /// names live in one arena that the index takes over.
    pub fn build(
        volumes: &[usn::Volume],
        entries: &[(u32, Vec<usn::RawEntry>)],
        names: String,
        exclusions: &Exclusions,
    ) -> Self {
        let mut sink = EntrySink::new(volumes);
        sink.index.names = names;
        let total: usize = entries.iter().map(|(_, list)| list.len()).sum();
        sink.reserve(total);
        for (volume_id, list) in entries {
            sink.begin_volume(*volume_id, false);
            for entry in list {
                sink.add_raw(entry);
            }
            sink.end_volume();
        }
        sink.finish(exclusions)
    }

    /// Build the index while the entries are still being read.
    ///
    /// The entries are handed over one at a time, so the flat list of them
    /// never exists on its own. That is what makes the difference between a
    /// peak of twice the size of the index and a peak of the size of the
    /// index, on a volume with millions of files.
    pub fn build_from(
        volumes: &[usn::Volume],
        exclusions: &Exclusions,
        feed: impl FnOnce(&mut EntrySink),
    ) -> Self {
        let mut sink = EntrySink::new(volumes);
        feed(&mut sink);
        sink.finish(exclusions)
    }

    /// Mark the entries that the user does not want to see.
    ///
    /// The check is done once, while the index is built, so that the query
    /// does not have to look at paths it is going to throw away anyway.
    pub(super) fn apply_exclusions(&mut self, exclusions: &Exclusions) {
        let count = self.entries.len();
        self.excluded = vec![false; count];
        if exclusions.is_empty() {
            return;
        }
        // The first pass marks the entries that are left out on their own.
        let mut own: Vec<bool> = vec![false; count];
        for index in 0..count {
            let name = self.name(index as u32);
            let by_extension =
                !self.entries[index].is_dir() && exclusions.excludes_extension(name);
            // The path is only needed when a folder is excluded, and building
            // it means walking up the tree and joining the parts.
            let by_folder = !exclusions.paths.is_empty()
                && exclusions.excludes_folder(&self.path(index as u32));
            own[index] = by_folder || by_extension;
        }
        // The second pass hands the mark down to the children. The entries are
        // walked from the top, so a folder is always handled before what is
        // inside it.
        //
        // The depth is a small number, so the entries are put in buckets
        // instead of being sorted: sorting millions of entries means
        // millions of random accesses, and the buckets are a single pass.
        let deepest = self
            .entries
            .iter()
            .map(|entry| entry.depth as usize)
            .max()
            .unwrap_or(0);
        let mut places = vec![0usize; deepest + 2];
        for entry in &self.entries {
            places[entry.depth as usize + 1] += 1;
        }
        for depth in 1..places.len() {
            places[depth] += places[depth - 1];
        }
        let mut order: Vec<u32> = vec![0; count];
        for (index, entry) in self.entries.iter().enumerate() {
            let depth = entry.depth as usize;
            order[places[depth]] = index as u32;
            places[depth] += 1;
        }
        for index in order {
            let parent = self.entries[index as usize].parent;
            if parent != NO_PARENT && self.excluded[parent as usize] {
                self.excluded[index as usize] = true;
            } else {
                self.excluded[index as usize] = own[index as usize];
            }
        }
    }

    /// Append one entry to the arena and the tree.
    pub(super) fn push_entry(&mut self, _volume: u32, record: &usn::RawEntry, parent: u32) -> u32 {
        let name_start = record.name_start;
        let name_len = record.name_len;
        self.entries.push(FsEntry {
            name_start,
            name_len: name_len.min(u16::MAX as u32) as u16,
            parent,
            attributes: record.attributes,
            depth: 0,
            size: record.size,
            created: record.created,
            modified: record.modified,
            accessed: record.accessed,
        });
        self.meta_known.push(record.meta_known);
        self.frns.push(record.frn);
        self.entries.len() as u32 - 1
    }

    /// The volume an entry belongs to.
    pub(super) fn volume_of(&self, index: u32) -> usize {
        self.volume_of_depth(index, 0)
    }

    /// The volume an entry belongs to.
    ///
    /// The entries that the change journal brought in are **outside** the
    /// ranges of the volumes — they were added at the end — so their volume
    /// is the one of the folder they are in.
    pub(super) fn volume_of_depth(&self, index: u32, depth: u32) -> usize {
        for range in &self.volume_ranges {
            if index >= range.start && index < range.end {
                return range.volume as usize;
            }
        }
        if depth > 8 {
            return 0;
        }
        let parent = self
            .entries
            .get(index as usize)
            .map(|entry| entry.parent)
            .unwrap_or(NO_PARENT);
        if parent == NO_PARENT || parent == index {
            return 0;
        }
        self.volume_of_depth(parent, depth + 1)
    }

    /// Work out how deep every entry sits.
    ///
    /// The depth is needed by the `depth:` filter and by the ranking, and
    /// walking the parent chain for a million entries on every keystroke
    /// would cost tens of milliseconds. It is computed once, here.
    pub(super) fn build_depths(&mut self) {
        // A depth of `u16::MAX` means "not worked out yet".
        for entry in &mut self.entries {
            entry.depth = u16::MAX;
        }
        // The chain is kept between the entries, so that walking over
        // millions of them does not allocate millions of small lists.
        let mut chain: Vec<u32> = Vec::new();
        for start in 0..self.entries.len() {
            if self.entries[start].depth != u16::MAX {
                continue;
            }
            // Collect the chain up to the first known depth.
            chain.clear();
            let mut current = start as u32;
            let mut guard = 0usize;
            let base = loop {
                if current == NO_PARENT {
                    break 0u16;
                }
                let entry = self.entries[current as usize];
                if entry.depth != u16::MAX {
                    break entry.depth;
                }
                chain.push(current);
                current = entry.parent;
                guard += 1;
                if guard > 256 {
                    break 0u16;
                }
            };
            // Write the depths back, deepest first.
            for (step, &index) in chain.iter().enumerate().rev() {
                let depth = base.saturating_add(step as u16 + 1);
                self.entries[index as usize].depth = depth;
            }
        }
    }

    /// Build the character filter of every entry.
    ///
    /// The mask holds the characters of the entry's own name, of its pinyin
    /// rendering and of the name of its folder, because a query can match
    /// any of the three.
    pub(super) fn build_masks(&mut self) {
        let count = self.entries.len();
        let mut own: Vec<u64> = Vec::with_capacity(count);
        for index in 0..count {
            own.push(self.own_mask_of(index as u32));
        }

        // The filter for a name query: the characters of the entry and of the
        // folder that holds it, because either can be matched.
        self.masks.reserve(count);
        for index in 0..count {
            let parent = self.entries[index].parent;
            let mut mask = own[index];
            if parent != NO_PARENT {
                mask |= own[parent as usize];
            }
            self.masks.push(mask);
        }

        // The filter for a path query: the characters of the entry and of
        // every folder above it, because a path query can match any of them.
        let mut path = vec![0u64; count];
        let mut known = vec![false; count];
        // The chain is kept between the entries, so that walking over
        // millions of them does not allocate millions of small lists.
        let mut chain: Vec<u32> = Vec::new();
        for start in 0..count {
            if known[start] {
                continue;
            }
            chain.clear();
            let mut current = start as u32;
            while current != NO_PARENT && !known[current as usize] && chain.len() < 256 {
                chain.push(current);
                current = self.entries[current as usize].parent;
            }
            let mut value = if current != NO_PARENT && known[current as usize] {
                path[current as usize]
            } else {
                0
            };
            for &index in chain.iter().rev() {
                value |= own[index as usize];
                path[index as usize] = value;
                known[index as usize] = true;
            }
        }
        self.path_masks = path;
    }

    /// Convert the names that contain Chinese characters.
    ///
    /// The pinyin is computed once, while the index is built, so that a query
    /// never has to translate anything.
    pub(super) fn build_pinyin(&mut self) {
        for index in 0..self.entries.len() {
            let name = self.name(index as u32);
            // A plain ASCII name cannot hold a Chinese character, and the
            // check for that is a fast byte scan.
            if name.is_ascii() || !name.chars().any(is_cjk) {
                continue;
            }
            if let Some(pinyin) = launcher_core::pinyin::PinyinIndex::build(name) {
                let rendered = pinyin.pinyin_lower().to_string();
                if !rendered.is_empty() {
                    self.pinyin.insert(index as u32, rendered);
                }
            }
        }
    }

    /// The full path of one entry.
    ///
    /// The tree is walked upwards and the parts are put back together, the
    /// same way Everything rebuilds a path from its folder pointers.
    pub fn path(&self, index: u32) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut current = index;
        let mut guard = 0usize;
        while current != NO_PARENT && guard < 256 {
            let name = self.name(current);
            // The root of a volume is called "." in the master file table,
            // and it is already the beginning of the path. Without this the
            // path would read `C:\.\Users\...`.
            if name != "." {
                parts.push(name);
            }
            current = self.entries[current as usize].parent;
            guard += 1;
        }
        let volume = self.volume_of(index);
        let root = self
            .volumes
            .get(volume)
            .map(|v| v.root.as_str())
            .unwrap_or("C:\\");
        let mut path = String::from(root);
        for part in parts.into_iter().rev() {
            if !path.ends_with('\\') {
                path.push('\\');
            }
            path.push_str(part);
        }
        path
    }

    /// The name of the folder that holds an entry.
    pub fn parent_name(&self, index: u32) -> &str {
        let parent = self.entries[index as usize].parent;
        if parent == NO_PARENT {
            ""
        } else {
            let name = self.name(parent);
            // The root of a volume is called "." — as a parent it is nothing.
            if name == "." {
                ""
            } else {
                name
            }
        }
    }

    /// The number of folders between the entry and its volume root.
    pub fn depth(&self, index: u32) -> u32 {
        self.entries[index as usize].depth as u32
    }

    /// Store the metadata of an entry.
    pub fn set_meta(&mut self, index: u32, meta: FileMeta) {
        self.meta.insert(index, meta);
    }

    /// The metadata of an entry, when it is known.
    pub fn meta(&self, index: u32) -> Option<&FileMeta> {
        self.meta.get(&index)
    }

    /// The moment the index is written, in seconds since the Unix epoch.
    ///
    /// The age of the index is counted from here, and the same number is
    /// written next to the index as the point in the change journal that the
    /// index was taken at — so that the two can be paired up again when the
    /// index is read back.
    pub fn stamp(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// The fingerprint of the contents of the index.
    ///
    /// Two indexes that hold the same files in the same order have the same
    /// fingerprint. It is the way to find out whether the table that was just
    /// read from the disk is the same as the one that was saved — and if it
    /// is, the saved one does not have to be written again. That write is the
    /// biggest one the program ever does: three quarters of a gigabyte for a
    /// big volume, and it is the one that wears the disk.
    pub fn fingerprint(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        {
            // The bytes are mixed in eight at a time: going over them one by
            // one would cost a second on a big volume, and the fingerprint is
            // only worth having if it is cheap.
            let mut mix = |bytes: &[u8]| {
                let mut words = bytes.chunks_exact(8);
                for word in &mut words {
                    hash ^= u64::from_le_bytes(word.try_into().unwrap());
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
                let rest = words.remainder();
                if !rest.is_empty() {
                    let mut tail = [0u8; 8];
                    tail[..rest.len()].copy_from_slice(rest);
                    hash ^= u64::from_le_bytes(tail);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            };
            mix(&(self.entries.len() as u64).to_le_bytes());
            for (index, entry) in self.entries.iter().enumerate() {
                if let Some(frn) = self.frns.get(index) {
                    mix(&frn.to_le_bytes());
                }
                mix(&entry.parent.to_le_bytes());
                mix(&entry.size.to_le_bytes());
                mix(&entry.created.to_le_bytes());
                mix(&entry.modified.to_le_bytes());
                mix(&entry.attributes.to_le_bytes());
            }
            // The names come last: they are the biggest part, and the entries
            // above say which name belongs to which entry.
            mix(self.names.as_bytes());
        }
        hash
    }

    /// The fingerprint of the table that was read back from the file.
    ///
    /// Zero means that there was no table — a table that was built in memory
    /// has no fingerprint yet.
    pub fn saved_fingerprint(&self) -> u64 {
        self.saved_fingerprint
    }

    /// Write the index to a file.
    ///
    /// The whole table of a big volume is tens of megabytes, so it is worth
    /// keeping: Everything does the same with its database, and without it
    /// every start would have to read the master file table again.
    ///
    /// The file is written to a temporary name first and then renamed, so an
    /// interrupted write cannot leave a half file behind.
    ///
    /// Returns the moment the index was written, so that the caller can
    /// write the point in the change journal with the very same number.
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<u64> {
        let stamp = self.stamp();
        let fingerprint = self.fingerprint();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        {
            // The file is written in pieces, so the whole table never has to
            // be in memory a second time. On a big volume the file is over
            // half a gigabyte.
            let file = std::fs::File::create(&tmp)?;
            io_priority::make_background(&file);
            let mut out = ChunkWriter::new(file);

            out.put(CACHE_MAGIC)?;
            out.put(&CACHE_VERSION.to_le_bytes())?;
            out.put(&stamp.to_le_bytes())?;
            out.put(&fingerprint.to_le_bytes())?;
            out.put(&(self.volumes.len() as u32).to_le_bytes())?;
            for volume in &self.volumes {
                for text in [&volume.root, &volume.device, &volume.label] {
                    let bytes = text.as_bytes();
                    out.put(&(bytes.len() as u16).to_le_bytes())?;
                    out.put(bytes)?;
                }
            }
            // Where the change journal of every volume stood when the table
            // was read. The table that is read back can be brought up to date
            // from there, without reading the disk again.
            out.put(&(self.journal_points.len() as u32).to_le_bytes())?;
            for point in &self.journal_points {
                out.put(&point.to_le_bytes())?;
            }
            out.put(&(self.volume_ranges.len() as u32).to_le_bytes())?;
            for range in &self.volume_ranges {
                out.put(&range.start.to_le_bytes())?;
                out.put(&range.end.to_le_bytes())?;
                out.put(&range.volume.to_le_bytes())?;
            }
            out.put(&(self.entries.len() as u64).to_le_bytes())?;
            // The bytes of one entry, put together once.
            let mut bytes = [0u8; 36];
            for entry in &self.entries {
                bytes[0..4].copy_from_slice(&entry.name_start.to_le_bytes());
                bytes[4..8].copy_from_slice(&entry.parent.to_le_bytes());
                bytes[8..16].copy_from_slice(&entry.size.to_le_bytes());
                bytes[16..18].copy_from_slice(&entry.name_len.to_le_bytes());
                bytes[18..20].copy_from_slice(&entry.depth.to_le_bytes());
                bytes[20..24].copy_from_slice(&entry.created.to_le_bytes());
                bytes[24..28].copy_from_slice(&entry.modified.to_le_bytes());
                bytes[28..32].copy_from_slice(&entry.accessed.to_le_bytes());
                bytes[32..36].copy_from_slice(&entry.attributes.to_le_bytes());
                out.put(&bytes)?;
            }
            out.put(&(self.names.len() as u64).to_le_bytes())?;
            out.put(self.names.as_bytes())?;
            let mut bytes = [0u8; 8];
            for mask in &self.masks {
                bytes.copy_from_slice(&mask.to_le_bytes());
                out.put(&bytes)?;
            }
            for mask in &self.path_masks {
                bytes.copy_from_slice(&mask.to_le_bytes());
                out.put(&bytes)?;
            }
            out.put(&(self.pinyin.len() as u32).to_le_bytes())?;
            let mut pinyin: Vec<(&u32, &String)> = self.pinyin.iter().collect();
            pinyin.sort_unstable();
            for (index, text) in pinyin {
                out.put(&index.to_le_bytes())?;
                out.put(&(text.len() as u16).to_le_bytes())?;
                out.put(text.as_bytes())?;
            }
            // The entries that are left out.
            out.put(&(self.excluded.len() as u64).to_le_bytes())?;
            for flag in &self.excluded {
                out.put(&[u8::from(*flag)])?;
            }
            // Which entries have a known size and time.
            out.put(&(self.meta_known.len() as u64).to_le_bytes())?;
            for flag in &self.meta_known {
                out.put(&[u8::from(*flag)])?;
            }
            // The file reference number of every entry.
            //
            // The change journal talks in these numbers, so without them the
            // index that was read back from the file could not be kept up to
            // date.
            out.put(&(self.frns.len() as u64).to_le_bytes())?;
            for frn in &self.frns {
                out.put(&frn.to_le_bytes())?;
            }
            out.finish()?.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(stamp)
    }

    /// Read an index back from a file.
    ///
    /// Returns `None` when the file is missing, of another version, or
    /// damaged. A damaged file is not an error: the index is rebuilt.
    pub fn load(path: &std::path::Path) -> Option<FsIndex> {
        let file = std::fs::File::open(path).ok()?;
        let mut reader = FileReader::new(file);
        let mut magic = [0u8; 4];
        reader.read_into(&mut magic)?;
        if magic != *CACHE_MAGIC {
            return None;
        }
        let version = reader.number(4)? as u32;
        if version != CACHE_VERSION {
            return None;
        }
        let built_at = reader.number(8)?;
        let saved_fingerprint = reader.number(8)?;

        let volume_count = reader.number(4)? as usize;
        let mut volumes = Vec::with_capacity(volume_count);
        for _ in 0..volume_count {
            volumes.push([
                reader.text()?,
                reader.text()?,
                reader.text()?,
            ]);
        }

        // Where the change journal of every volume stood when the table was
        // read. Without it the table would have to be read again.
        let point_count = reader.number(4)? as usize;
        let mut journal_points = Vec::with_capacity(point_count);
        for _ in 0..point_count {
            journal_points.push(reader.number(8)?);
        }

        let range_count = reader.number(4)? as usize;
        let mut volume_ranges = Vec::with_capacity(range_count);
        for _ in 0..range_count {
            volume_ranges.push(VolumeRange {
                start: reader.number(4)? as u32,
                end: reader.number(4)? as u32,
                volume: reader.number(4)? as u32,
            });
        }

        let entry_count = reader.number(8)? as usize;
        // The entries are read in pieces, so the file never has to be in
        // memory as a whole.
        let mut entries = Vec::with_capacity(entry_count);
        let mut bytes = [0u8; 36];
        while entries.len() < entry_count {
            let take = (entry_count - entries.len()).min(4096);
            for _ in 0..take {
                reader.read_into(&mut bytes)?;
                entries.push(FsEntry {
                    name_start: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
                    parent: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                    size: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
                    name_len: u16::from_le_bytes(bytes[16..18].try_into().unwrap()),
                    depth: u16::from_le_bytes(bytes[18..20].try_into().unwrap()),
                    created: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
                    modified: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
                    accessed: u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
                    attributes: u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
                });
            }
        }

        let names_len = reader.number(8)? as usize;
        // The names are read straight into the arena of the index, so they are
        // not in memory twice.
        let mut names = String::with_capacity(names_len);
        let read = unsafe {
            let buffer = names.as_mut_vec();
            buffer.resize(names_len, 0);
            reader.read_into(buffer).is_some()
        };
        if !read {
            return None;
        }
        // The names come from the file system, so they are text. A damaged
        // file is not an error: the index is rebuilt.
        if std::str::from_utf8(names.as_bytes()).is_err() {
            let bytes = names.as_bytes().to_vec();
            names = String::from_utf8_lossy(&bytes).to_string();
        }

        let mut masks = Vec::with_capacity(entry_count);
        let mut bytes = [0u8; 8];
        while masks.len() < entry_count {
            let take = (entry_count - masks.len()).min(4096);
            for _ in 0..take {
                reader.read_into(&mut bytes)?;
                masks.push(u64::from_le_bytes(bytes));
            }
        }
        let mut path_masks = Vec::with_capacity(entry_count);
        while path_masks.len() < entry_count {
            let take = (entry_count - path_masks.len()).min(4096);
            for _ in 0..take {
                reader.read_into(&mut bytes)?;
                path_masks.push(u64::from_le_bytes(bytes));
            }
        }
        let pinyin_count = reader.number(4)? as usize;
        let mut pinyin = launcher_core::hash::fast_map_with_capacity(pinyin_count);
        for _ in 0..pinyin_count {
            let index = reader.number(4)? as u32;
            let text = reader.text()?;
            pinyin.insert(index, text);
        }
        let excluded_len = reader.number(8)? as usize;
        let mut excluded = Vec::with_capacity(excluded_len);
        while excluded.len() < excluded_len {
            let take = (excluded_len - excluded.len()).min(4096);
            let mut chunk = [0u8; 4096];
            reader.read_into(&mut chunk[..take])?;
            for flag in &chunk[..take] {
                excluded.push(*flag != 0);
            }
        }
        let known_len = reader.number(8)? as usize;
        let mut meta_known = Vec::with_capacity(known_len);
        while meta_known.len() < known_len {
            let take = (known_len - meta_known.len()).min(4096);
            let mut chunk = [0u8; 4096];
            reader.read_into(&mut chunk[..take])?;
            for flag in &chunk[..take] {
                meta_known.push(*flag != 0);
            }
        }
        // The file reference number of every entry: the change journal talks
        // in these numbers, so the index that was read back can be kept up to
        // date.
        let frn_len = reader.number(8)? as usize;
        let mut frns = Vec::with_capacity(frn_len);
        while frns.len() < frn_len {
            let take = (frn_len - frns.len()).min(4096);
            let mut chunk = [0u8; 8 * 4096];
            reader.read_into(&mut chunk[..take * 8])?;
            for number in chunk[..take * 8].chunks_exact(8) {
                frns.push(u64::from_le_bytes(number.try_into().unwrap()));
            }
        }

        let mut index = FsIndex {
            names,
            entries,
            volumes: volumes
                .into_iter()
                .map(|parts| IndexedVolume {
                    root: parts[0].clone(),
                    device: parts[1].clone(),
                    label: parts[2].clone(),
                })
                .collect(),
            volume_ranges,
            masks,
            path_masks,
            meta: launcher_core::hash::fast_map(),
            pinyin,
            built_at,
            fresh_at: built_at,
            saved_fingerprint,
            excluded,
            meta_known,
            frns,
            journal_points,
            frn_index: launcher_core::hash::fast_map(),
            frns_in_order: true,
            name_counts: std::sync::OnceLock::new(),
            children: std::sync::OnceLock::new(),
        };
        // The table was read in the order of the numbers, so they are in
        // order and a file can be found by a binary search. The check is one
        // pass, and it saves the map of nine million entries.
        index.frns_in_order = index.frns_are_in_order();
        Some(index)
    }

    /// When the index was built, in seconds since the Unix epoch.
    pub fn built_at(&self) -> u64 {
        self.built_at
    }

    /// Where the change journal of every volume stood when the table was read.
    ///
    /// The numbers belong to the volumes of the table, in the same order. A
    /// number of zero means that the point is not known for that volume.
    pub fn journal_points(&self) -> &[u64] {
        &self.journal_points
    }

    /// Say where the change journal of every volume stands now.
    pub fn set_journal_points(&mut self, points: Vec<u64>) {
        self.journal_points = points;
    }

    /// Say when the index was last brought up to date.
    ///
    /// The table that was saved is built once; the changes that came after it
    /// are applied to it when it is read back. The age of the index is the
    /// age of those changes.
    pub fn set_fresh_at(&mut self, stamp: u64) {
        self.fresh_at = stamp;
    }

    /// How old the index is.
    ///
    /// The age is the age of the **changes** that were applied to the saved
    /// table, not the age of the table itself: the table may have been built
    /// weeks ago and brought up to date every few hours since.
    pub fn age(&self) -> std::time::Duration {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stamp = if self.fresh_at != 0 {
            self.fresh_at
        } else {
            self.built_at
        };
        std::time::Duration::from_secs(now.saturating_sub(stamp))
    }

    /// The names of the volumes the index covers.
    pub fn volume_signature(&self) -> Vec<String> {
        self.volumes
            .iter()
            .map(|v| format!("{}|{}", v.device, v.root))
            .collect()
    }

    /// How many entries share a name.
    ///
    /// It is built the first time a query asks for the duplicates.
    pub(super) fn name_counts(&self) -> &HashMap<String, u32> {
        self.name_counts.get_or_init(|| {
            let mut counts: HashMap<String, u32> = HashMap::with_capacity(self.entries.len());
            for index in 0..self.entries.len() as u32 {
                *counts
                    .entry(self.name(index).to_lowercase())
                    .or_insert(0) += 1;
            }
            counts
        })
    }

    /// The children of every folder.
    ///
    /// It is built the first time a query asks for the children.
    pub(super) fn child_map(&self) -> &HashMap<u32, Vec<u32>> {
        self.children.get_or_init(|| {
            let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
            for (index, entry) in self.entries.iter().enumerate() {
                map.entry(entry.parent).or_default().push(index as u32);
            }
            map
        })
    }

    /// Bring the index up to date with the changes of the change journal.
    ///
    /// This is what keeps the index alive while the program runs
    /// (`_db_monitor_ntfs_process_usn_records` in Everything):
    ///
    /// * a file that was created is added,
    /// * a file that was deleted is taken out,
    /// * a file that was renamed gets its new name.
    ///
    /// Returns how many entries changed.
    pub fn apply_changes(&mut self, changes: &[usn::JournalChange]) -> usize {
        self.apply_changes_collecting(changes, &mut Vec::new())
    }

    /// Bring the index up to date with the changes of the change journal,
    /// and say **which of the changes actually changed the table**.
    ///
    /// Only those are worth writing down. A program that rewrites the same
    /// file a thousand times leaves a thousand records in the journal, and
    /// **none of them changes the table** — the file is there, with the same
    /// name, in the same folder, and the journal does not even carry its
    /// size and its time.
    ///
    /// Writing them down anyway would make the changes file a hundred times
    /// bigger than it has to be — and it is written again from the beginning
    /// every time, so the cost is paid over and over.
    pub fn apply_changes_collecting(
        &mut self,
        changes: &[usn::JournalChange],
        important: &mut Vec<IndexChange>,
    ) -> usize {
        if changes.is_empty() {
            return 0;
        }
        if !self.frns_in_order {
            self.ensure_frn_index();
        }
        let mut touched = 0usize;
        for change in changes {
            let existing = self.entry_of_frn(change.frn);
            match (change.reason & usn::reason::FILE_CREATE != 0, existing) {
                (true, None) => {
                    self.add_entry(change);
                    important.push(journal_change(change));
                    touched += 1;
                }
                (false, Some(index)) => {
                    if change.reason & usn::reason::FILE_DELETE != 0 {
                        self.remove_entry(index);
                        important.push(journal_change(change));
                        touched += 1;
                    } else if change.reason & usn::reason::RENAME_NEW_NAME != 0 {
                        self.rename_entry(index, &change.name);
                        important.push(journal_change(change));
                        touched += 1;
                    }
                }
                (true, Some(_)) => {}
                (false, None) => {}
            }
        }
        touched
    }

    /// The changes that turn `older` into this index.
    ///
    /// Both indexes hold the entries of the master file table in the order of
    /// their record numbers, so the two are walked side by side: the walk
    /// costs one pass and no map of nine million entries.
    ///
    /// The result is what is written between two full tables: the whole table
    /// is three quarters of a gigabyte, and the changes between two of them
    /// are a few megabytes at most.
    ///
    /// The walk stops after `limit` changes. A table that was read back
    /// wrongly (a different volume, a different exclusion) would otherwise
    /// say that **every** entry changed — and nine million changes are nine
    /// hundred megabytes of memory. When the limit is reached, the caller
    /// writes the whole table again, which is the right answer for a table
    /// that really did change completely.
    pub fn changes_since(&self, older: &FsIndex) -> Vec<IndexChange> {
        self.changes_since_limited(older, 200_000).0
    }

    /// The changes that turn `older` into this index, up to a limit.
    ///
    /// Returns the changes and whether the limit was reached.
    pub fn changes_since_limited(
        &self,
        older: &FsIndex,
        limit: usize,
    ) -> (Vec<IndexChange>, bool) {
        let mut changes = Vec::new();
        // The two tables are compared by the **number of the record**, never
        // by the place: a table that was read back can have its entries in
        // another order than a table that was just read (the files that the
        // change journal brought in are put at the end), and comparing them
        // by their places would say that *everything* changed — and then the
        // whole table would be written again for nothing.
        let old_order = older.sorted_by_number();
        let new_order = self.sorted_by_number();
        // The files that are new, and the files that changed.
        for index in new_order.iter().copied() {
            let number = self.frns[index as usize] & RECORD_NUMBER_MASK;
            let old = older.index_of_number(&old_order, number);
            let changed = match old {
                Some(old) => self.entry_differs_from(older, old as usize, index as usize),
                None => true,
            };
            if changed {
                changes.push(self.change_of(index as usize, false));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        // The files that are gone.
        for index in old_order.iter().copied() {
            if older.is_gone(index as usize) {
                continue;
            }
            let number = older.frns[index as usize] & RECORD_NUMBER_MASK;
            if self.index_of_number(&new_order, number).is_none() {
                changes.push(older.change_of(index as usize, true));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        // The files that the change journal brought in are at the end of a
        // table and are not in the order of the record numbers, so they are
        // looked at one by one.
        for index in self.mft_end()..self.entries.len() {
            let number = self.frns[index] & RECORD_NUMBER_MASK;
            let changed = match older.entry_of_frn(number) {
                Some(old) => self.entry_differs_from(older, old as usize, index),
                None => true,
            };
            if changed {
                changes.push(self.change_of(index, false));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        for index in older.mft_end()..older.entries.len() {
            if older.is_gone(index) {
                continue;
            }
            let number = older.frns[index] & RECORD_NUMBER_MASK;
            // The file is still there — the change was already reported by the
            // loop above, which looks every file of the new table up in the old
            // one. Only the files that are really gone are reported here.
            if self.entry_of_frn(number).is_none() {
                changes.push(older.change_of(index, true));
                if changes.len() > limit {
                    return (changes, true);
                }
            }
        }
        (changes, false)
    }

    /// The entries of the master file table, in the order of their record
    /// numbers.
    ///
    /// The table is read in that order, so the list is usually already in it
    /// and nothing has to be done.
    pub(super) fn sorted_by_number(&self) -> Vec<u32> {
        let mut order: Vec<u32> = (0..self.mft_end() as u32).collect();
        let mut sorted = true;
        for pair in order.windows(2) {
            let left = self.frns[pair[0] as usize] & RECORD_NUMBER_MASK;
            let right = self.frns[pair[1] as usize] & RECORD_NUMBER_MASK;
            if left >= right {
                sorted = false;
                break;
            }
        }
        if !sorted {
            order.sort_unstable_by_key(|index| self.frns[*index as usize] & RECORD_NUMBER_MASK);
        }
        order
    }

    /// The entry with the given record number, in a list that is sorted by it.
    pub(super) fn index_of_number(&self, order: &[u32], number: u64) -> Option<u32> {
        order
            .binary_search_by(|index| {
                (self.frns[*index as usize] & RECORD_NUMBER_MASK).cmp(&number)
            })
            .ok()
            .map(|at| order[at])
    }

    /// Whether the entry is one that is not there any more.
    ///
    /// A file that was deleted is left in the table with a mark on it, so that
    /// the other entries keep their places. Such a file is not reported as a
    /// change again — it was reported when it was deleted.
    pub(super) fn is_gone(&self, index: usize) -> bool {
        self.excluded.get(index).copied().unwrap_or(false)
    }

    /// How many entries of the index came from the master file table.
    ///
    /// They are the first ones, and they are in the order of the record
    /// numbers. The files that the change journal brought in follow them.
    pub(super) fn mft_end(&self) -> usize {
        self.volume_ranges
            .iter()
            .map(|range| range.end as usize)
            .max()
            .unwrap_or(0)
            .min(self.frns.len())
    }

    /// The change that turns the entry `index` into something else.
    ///
    /// With `deleted` the file is gone, without it the file has these values.
    pub(super) fn change_of(&self, index: usize, deleted: bool) -> IndexChange {
        let entry = &self.entries[index];
        IndexChange {
            frn: self.frns[index],
            parent_frn: self.parent_frn_of(index),
            name: self.name(index as u32).to_string(),
            deleted,
            size: entry.size,
            created: entry.created,
            modified: entry.modified,
            accessed: entry.accessed,
            attributes: entry.attributes,
            meta_known: true,
        }
    }

    /// The file reference number of the folder that holds an entry.
    pub(super) fn parent_frn_of(&self, index: usize) -> u64 {
        let parent = self.entries[index].parent;
        if parent == NO_PARENT {
            return 0;
        }
        self.frns
            .get(parent as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Whether the entry `older` is another file than the entry `new`.
    pub(super) fn entry_differs_from(&self, older: &FsIndex, older_index: usize, new_index: usize) -> bool {
        let old_frn = older.frns[older_index];
        let new_frn = self.frns[new_index];
        // A different sequence means that the record was used again: it is a
        // different file now.
        if (old_frn >> 48) != (new_frn >> 48) {
            return true;
        }
        let old_entry = &older.entries[older_index];
        let new_entry = &self.entries[new_index];
        if older.name(older_index as u32) != self.name(new_index as u32) {
            return true;
        }
        // A file that was moved to another folder keeps its name and its
        // size, so the folder it is in has to be looked at as well.
        if older.parent_frn_of(older_index) != self.parent_frn_of(new_index) {
            return true;
        }
        old_entry.size != new_entry.size
            || old_entry.created != new_entry.created
            || old_entry.modified != new_entry.modified
            || old_entry.attributes != new_entry.attributes
    }

    /// Bring the index up to date with the changes that were written down.
    ///
    /// This is the counterpart of [`FsIndex::changes_since`]: the changes
    /// that are read back are applied to the table that was saved, and the
    /// result is the table as it was when the changes were written.
    pub fn apply_index_changes(&mut self, changes: &[IndexChange]) -> usize {
        if changes.is_empty() {
            return 0;
        }
        if !self.frns_in_order {
            self.ensure_frn_index();
        }
        let mut touched = 0usize;
        for change in changes {
            let existing = self.entry_of_frn(change.frn);
            match (change.deleted, existing) {
                (true, Some(index)) => {
                    self.remove_entry(index);
                    touched += 1;
                }
                (true, None) => {}
                (false, Some(index)) => {
                    if self.name(index) != change.name {
                        self.rename_entry(index, &change.name);
                    }
                    // The record may have been used again since the table was
                    // written: the number of the record is the same, but the
                    // sequence is a new one — and the sequence is what tells
                    // the two files apart.
                    self.frns[index as usize] = change.frn;
                    self.frn_index.insert(change.frn, index);
                    // The file is there again: it was deleted before and the
                    // record was used again. The mark of the deletion has to
                    // go, or the file would stay invisible for ever.
                    self.excluded[index as usize] = false;
                    if change.meta_known {
                        let entry = &mut self.entries[index as usize];
                        entry.size = change.size;
                        entry.created = change.created;
                        entry.modified = change.modified;
                        entry.accessed = change.accessed;
                        entry.attributes = change.attributes;
                    }
                    self.refresh_entry(index);
                    touched += 1;
                }
                (false, None) => {
                    self.add_index_entry(change);
                    touched += 1;
                }
            }
        }
        touched
    }

    /// Add a file that is not in the index yet.
    pub(super) fn add_index_entry(&mut self, change: &IndexChange) {
        // The folder has to be there; when it is not, the entry is dropped,
        // because its place in the tree would be unknown.
        let parent = self
            .entry_of_frn(change.parent_frn)
            .unwrap_or(NO_PARENT);
        let name_start = self.names.len() as u32;
        self.names.push_str(&change.name);
        let index = self.entries.len() as u32;
        self.entries.push(FsEntry {
            name_start,
            name_len: change.name.len().min(u16::MAX as usize) as u16,
            parent,
            attributes: change.attributes,
            depth: 0,
            size: change.size,
            created: change.created,
            modified: change.modified,
            accessed: change.accessed,
        });
        self.excluded.push(false);
        self.meta_known.push(false);
        self.frns.push(change.frn);
        self.frn_index.insert(change.frn, index);
        self.masks.push(0);
        self.path_masks.push(0);
        self.refresh_entry(index);
    }

    /// Write the changes since the saved table to a file.
    ///
    /// The number of the saved table comes with them, so that changes that
    /// belong to another table are not used.
    pub fn save_changes(
        &self,
        path: &std::path::Path,
        base_stamp: u64,
        changes: &[IndexChange],
    ) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let stamp = self.stamp();
        let tmp = path.with_extension("tmp");
        {
            let file = std::fs::File::create(&tmp)?;
            io_priority::make_background(&file);
            let mut out = ChunkWriter::new(file);
            out.put(DELTA_MAGIC)?;
            out.put(&DELTA_VERSION.to_le_bytes())?;
            out.put(&base_stamp.to_le_bytes())?;
            out.put(&stamp.to_le_bytes())?;
            out.put(&(changes.len() as u64).to_le_bytes())?;
            for change in changes {
                out.put(&change.frn.to_le_bytes())?;
                out.put(&change.parent_frn.to_le_bytes())?;
                out.put(&change.size.to_le_bytes())?;
                out.put(&change.created.to_le_bytes())?;
                out.put(&change.modified.to_le_bytes())?;
                out.put(&change.accessed.to_le_bytes())?;
                out.put(&change.attributes.to_le_bytes())?;
                out.put(&[u8::from(change.deleted)])?;
                out.put(&[u8::from(change.meta_known)])?;
                let name = change.name.as_bytes();
                out.put(&(name.len() as u16).to_le_bytes())?;
                out.put(name)?;
            }
            out.finish()?.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Read the changes since the saved table back.
    ///
    /// Returns `None` when the file is missing, of another version, or does
    /// not belong to the table that was read. The number that comes with the
    /// changes is the moment they were written.
    pub fn load_changes(path: &std::path::Path, base_stamp: u64) -> Option<(u64, Vec<IndexChange>)> {
        let file = std::fs::File::open(path).ok()?;
        let mut reader = FileReader::new(file);
        let mut magic = [0u8; 4];
        reader.read_into(&mut magic)?;
        if magic != *DELTA_MAGIC {
            return None;
        }
        let version = reader.number(4)? as u32;
        if version != DELTA_VERSION {
            return None;
        }
        let stamp = reader.number(8)?;
        if stamp != base_stamp {
            // The changes belong to another table, so they are of no use.
            return None;
        }
        let written_at = reader.number(8)?;
        let count = reader.number(8)? as usize;
        let mut changes = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let frn = reader.number(8)?;
            let parent_frn = reader.number(8)?;
            let size = reader.number(8)?;
            let created = reader.number(4)? as u32;
            let modified = reader.number(4)? as u32;
            let accessed = reader.number(4)? as u32;
            let attributes = reader.number(4)? as u32;
            let deleted = reader.number(1)? != 0;
            let meta_known = reader.number(1)? != 0;
            let name = reader.text()?;
            changes.push(IndexChange {
                frn,
                parent_frn,
                name,
                deleted,
                size,
                created,
                modified,
                accessed,
                attributes,
                meta_known,
            });
        }
        Some((written_at, changes))
    }

    /// The entry of a file reference number.
    ///
    /// The number of a record is 64 bits, but the high 16 of them are the
    /// **sequence** of the record — it grows every time the record is used
    /// again. The low 48 bits are the number of the record itself.
    ///
    /// **The whole 64 bits are the name of the file.** A file that is deleted
    /// and created again keeps its record number and gets a new sequence —
    /// and then it is a different file. Looking only at the low half would
    /// find the old file and take the new one for it, and the new file would
    /// never show up.
    pub(super) fn entry_of_frn(&self, frn: u64) -> Option<u32> {
        let number = frn & RECORD_NUMBER_MASK;
        if !self.frns_in_order {
            return self.frn_index.get(&frn).copied();
        }
        // The files that the change journal brought in are few, and they are
        // in the map.
        if let Some(index) = self.frn_index.get(&frn) {
            return Some(*index);
        }
        // The files of the master file table are found by a binary search:
        // their numbers are in order, one volume after the other.
        for range in &self.volume_ranges {
            let start = range.start as usize;
            let end = (range.end as usize).min(self.frns.len());
            if start >= end {
                continue;
            }
            let found = self.frns[start..end]
                .binary_search_by(|probe| (probe & RECORD_NUMBER_MASK).cmp(&number));
            if let Ok(position) = found {
                let index = start + position;
                // The record may have been used again since the table was
                // read: the number is the same, but the sequence is a new one
                // — and then it is another file.
                if self.frns[index] == frn {
                    return Some(index as u32);
                }
                return None;
            }
        }
        None
    }

    /// The map from the file reference number to the entry, built on demand.
    ///
    /// It is only needed when the numbers are **not** in order — then the
    /// binary search cannot be used and every entry has to be looked up.
    pub(super) fn ensure_frn_index(&mut self) {
        if !self.frn_index.is_empty() {
            return;
        }
        let mut map = launcher_core::hash::fast_map_with_capacity(self.entries.len());
        for (index, frn) in self.frns.iter().enumerate() {
            map.insert(*frn, index as u32);
        }
        self.frn_index = map;
    }

    /// Whether the file reference numbers are in order, so that a file can be
    /// found by a binary search.
    ///
    /// The master file table is read in the order of the numbers, so they
    /// are. The check is one pass over the numbers, and it saves the two
    /// hundred megabytes of memory that the map of nine million entries
    /// would take.
    pub fn frns_are_in_order(&self) -> bool {
        for range in &self.volume_ranges {
            let start = range.start as usize;
            let end = (range.end as usize).min(self.frns.len());
            for index in start..end.saturating_sub(1) {
                if (self.frns[index] & RECORD_NUMBER_MASK) >= (self.frns[index + 1] & RECORD_NUMBER_MASK)
                {
                    return false;
                }
            }
        }
        true
    }

    /// Add a file that was created.
    pub(super) fn add_entry(&mut self, change: &usn::JournalChange) {
        // The folder has to be there; when it is not, the entry is dropped,
        // because its place in the tree would be unknown.
        let parent = self
            .entry_of_frn(change.parent_frn)
            .unwrap_or(NO_PARENT);
        let name_start = self.names.len() as u32;
        self.names.push_str(&change.name);
        let index = self.entries.len() as u32;
        self.entries.push(FsEntry {
            name_start,
            name_len: change.name.len().min(u16::MAX as usize) as u16,
            parent,
            attributes: change.attributes,
            depth: 0,
            size: 0,
            created: 0,
            modified: 0,
            accessed: 0,
        });
        self.excluded.push(false);
        self.meta_known.push(false);
        self.frns.push(change.frn);
        self.frn_index.insert(change.frn, index);
        self.masks.push(0);
        self.path_masks.push(0);
        self.refresh_entry(index);
    }

    /// Take a file that was deleted out of the index.
    pub(super) fn remove_entry(&mut self, index: u32) {
        self.excluded[index as usize] = true;
        self.frn_index.remove(&self.frns[index as usize]);
    }

    /// Give a file its new name.
    pub(super) fn rename_entry(&mut self, index: u32, name: &str) {
        let name_start = self.names.len() as u32;
        self.names.push_str(name);
        let entry = &mut self.entries[index as usize];
        entry.name_start = name_start;
        entry.name_len = name.len().min(u16::MAX as usize) as u16;
        self.refresh_entry(index);
    }

    /// Recompute what depends on one entry: the depth, the filter masks and
    /// the pinyin.
    pub(super) fn refresh_entry(&mut self, index: u32) {
        // The depth.
        let mut depth = 0u16;
        let mut current = self.entries[index as usize].parent;
        let mut guard = 0usize;
        while current != NO_PARENT && guard < 256 {
            depth = depth.saturating_add(1);
            current = self.entries[current as usize].parent;
            guard += 1;
        }
        self.entries[index as usize].depth = depth;

        // The pinyin.
        let name = self.name(index).to_string();
        self.pinyin.remove(&index);
        if name.chars().any(is_cjk) {
            if let Some(pinyin) = launcher_core::pinyin::PinyinIndex::build(&name) {
                let rendered = pinyin.pinyin_lower().to_string();
                if !rendered.is_empty() {
                    self.pinyin.insert(index, rendered);
                }
            }
        }

        // The filter masks.
        let mut own = name_mask(&name);
        if let Some(pinyin) = self.pinyin.get(&index) {
            own |= name_mask(pinyin);
        }
        let parent = self.entries[index as usize].parent;
        let parent_mask = if parent == NO_PARENT {
            0
        } else {
            self.masks[parent as usize]
        };
        self.masks[index as usize] = own | parent_mask;

        let mut path_mask = own;
        let mut current = parent;
        let mut guard = 0usize;
        while current != NO_PARENT && guard < 256 {
            path_mask |= self.masks[current as usize];
            current = self.entries[current as usize].parent;
            guard += 1;
        }
        self.path_masks[index as usize] = path_mask;
    }

    /// The characters every plain term of the query requires.
    ///
    /// A term can only match a name that has all of them, so the filter of
    /// a folder tells whether one of the terms could match it.
    pub(super) fn term_masks(&self, parsed: &search_syntax::ParsedSearch) -> Vec<u64> {
        let wildcards = parsed.options.wildcards;
        search_syntax::terms(&parsed.node)
            .into_iter()
            .map(|term| term_mask(term, wildcards))
            .filter(|mask| *mask != 0)
            .collect()
    }

    /// Whether the folder of an entry could match one of the terms.
    ///
    /// The filter of the folder holds the characters of its name, so if one
    /// of the terms needs a character the folder does not have, that term
    /// cannot match it — and the name of the folder does not have to be
    /// looked up at all.
    pub(super) fn folder_can_match(&self, index: u32, term_masks: &[u64], filters: &[u64]) -> bool {
        if term_masks.is_empty() {
            return true;
        }
        let parent = self.entries[index as usize].parent;
        if parent == NO_PARENT {
            return false;
        }
        let folder = filters[parent as usize];
        term_masks
            .iter()
            .any(|mask| folder & mask == *mask)
    }

    /// Search the index.
    ///
    /// The query uses the Everything syntax, see
    /// [`launcher_core::search_syntax`]. The results are ranked by
    /// relevance rather than by name.
    pub fn search(&self, query: &str, limit: usize) -> Vec<FsHit> {
        self.search_with(query, limit, None, FileSort::default())
    }

    /// How many entries would survive the character filter of the query.
    ///
    /// This is the size of the set that the real evaluation has to walk
    /// through, so it says how much work a query is: a single letter passes
    /// millions of entries, a word with rare letters passes a handful.
    ///
    /// It is a diagnostic: the count is taken by walking the table of the
    /// filters once, which is the same work the search does.
    pub fn candidate_count(&self, query: &str) -> usize {
        let parsed = ParsedSearch::parse(query);
        if parsed.is_empty() {
            return 0;
        }
        let Some(mask) = required_mask(&parsed.node, parsed.options.wildcards) else {
            return 0;
        };
        if mask == 0 {
            return 0;
        }
        let filters = if parsed.options.match_path {
            &self.path_masks
        } else {
            &self.masks
        };
        filters
            .par_iter()
            .zip(self.excluded.par_iter())
            .filter(|(entry_mask, excluded)| !**excluded && **entry_mask & mask == mask)
            .count()
    }

    /// How many entries have a different mask in the two tables.
    ///
    /// The mask of an entry holds the characters of its own name **and** of
    /// the names above it, so the two tables should hold the same thing. If
    /// they do, one of them is a copy of the other and can go.
    ///
    /// It is a diagnostic.
    pub fn mask_divergence(&self) -> usize {
        self.masks
            .iter()
            .zip(self.path_masks.iter())
            .filter(|(own, path)| own != path)
            .count()
    }

    /// Search the index, fetching metadata for the candidates that need it.
    ///
    /// The provider is only called when the query filters on the size or on a
    /// timestamp, and only for the candidates that survived the character
    /// filter. That keeps the cost of `size:>1mb report` down to the handful
    /// of files whose name matches.
    pub fn search_with(
        &self,
        query: &str,
        limit: usize,
        metadata: Option<&(dyn Fn(u32) -> Option<FileMeta> + Sync)>,
        sort: FileSort,
    ) -> Vec<FsHit> {
        self.search_with_cancel(query, limit, metadata, sort, None)
    }

    /// Search the index, and stop early when the query goes out of date.
    ///
    /// One keystroke scans the whole table, and the next keystroke usually
    /// comes a few hundred milliseconds later. The result of the older query
    /// is of no use to anybody by then, so it stops as soon as it sees that a
    /// newer query has started.
    ///
    /// The token and the generation are the ones the query was started with.
    pub fn search_with_cancel(
        &self,
        query: &str,
        limit: usize,
        metadata: Option<&(dyn Fn(u32) -> Option<FileMeta> + Sync)>,
        sort: FileSort,
        cancel: Option<(&Cancel, u64)>,
    ) -> Vec<FsHit> {
        let parsed = {
            let _timer = launcher_core::profile::phase("index.fs.parse");
            ParsedSearch::parse(query)
        };
        if parsed.is_empty() {
            return Vec::new();
        }
        let term = search_syntax::primary_term(&parsed.node).cloned();
        // Sorting by the size or by the time needs the metadata as well.
        let wants_metadata = search_syntax::needs_metadata(&parsed.node)
            || matches!(sort, FileSort::Size | FileSort::Modified);
        // The duplicates and the children cannot be answered by looking at one
        // entry alone, so the index does the work itself.
        let duplicates = search_syntax::function_values(&parsed.node, "dupe");
        let children = search_syntax::function_values(&parsed.node, "child");

        // ---- the cheap filter ------------------------------------------------
        //
        // The characters a name has to have for the query to match. A query
        // that matches the whole path is checked against the filter that
        // holds the characters of every folder on the way down.
        let filter = required_mask(&parsed.node, parsed.options.wildcards);
        let filters = if parsed.options.match_path {
            &self.path_masks
        } else {
            &self.masks
        };

        // ---- the real evaluation ---------------------------------------------
        //
        // Only a score per candidate is kept here; the names and paths are
        // built for the few that survive, so the memory does not grow with
        // the size of the index.
        let options = parsed.options;
        // The characters every term needs. They tell whether the folder of a
        // candidate could match, and looking up the name of the folder is one
        // of the more expensive things a candidate costs.
        let term_masks = self.term_masks(&parsed);
        let score_of = |index: u32| -> Option<(i32, u32, u16)> {
            let name = self.name(index);
            let target = IndexTarget {
                index: self,
                entry: index,
                name,
                attributes: self.entries[index as usize].attributes,
                depth: self.depth(index),
                // Only a name with Chinese characters in it has a pinyin
                // rendering, and such a name is never plain ASCII. The check
                // keeps the lookup in the table of the pinyin renderings away
                // from the other candidates.
                alias: if name.is_ascii() {
                    None
                } else {
                    self.pinyin.get(&index).map(|text| text.as_str())
                },
                term_masks: &term_masks,
                filters,
                meta: if wants_metadata {
                    // The size and the times come from the table when it was
                    // read straight off the disk; only the entries from the
                    // change journal have to be looked up.
                    let known = self.meta.get(&index).copied().or_else(|| {
                        if self.meta_known.get(index as usize).copied().unwrap_or(false) {
                            self.entries[index as usize].meta()
                        } else {
                            None
                        }
                    });
                    known.or_else(|| metadata.and_then(|fetch| fetch(index)))
                } else {
                    None
                },
            };
            // The match and the score come out of the same walk over the
            // name, so a candidate is only looked at once.
            let quality = search_syntax::evaluate_scored(&parsed.node, &target, &options)?;

            // The duplicates: the name has to occur more than once.
            if !duplicates.is_empty() {
                let counts = self.name_counts();
                let name = target.name().to_lowercase();
                let shared = counts.get(&name).copied().unwrap_or(0);
                if shared < 2 {
                    return None;
                }
                // A value narrows the search down to a certain name.
                if let Some(wanted) = duplicates.first() {
                    if !wanted.is_empty()
                        && !search_syntax::contains_fold(&name, &wanted.to_lowercase())
                    {
                        return None;
                    }
                }
            }
            // The children: the folder has to hold a child with that name.
            if !children.is_empty() {
                if !target.is_dir() {
                    return None;
                }
                let map = self.child_map();
                let found = map.get(&index).map(|list| {
                    list.iter().any(|child| {
                        let name = self.name(*child).to_lowercase();
                        children.iter().any(|wanted| {
                            wanted.is_empty()
                                || search_syntax::contains_fold(&name, &wanted.to_lowercase())
                        })
                    })
                });
                if found != Some(true) {
                    return None;
                }
            }
            // Shorter names are more likely to be what the user meant.
            let bonus = (200 - (target.name().len() as i32)).clamp(0, 200);
            let score = if term.is_some() {
                quality.min(search_syntax::SCORE_EXACT) + bonus
            } else {
                bonus
            };
            Some((score, index, self.entries[index as usize].name_len))
        };

        // The ranking of the candidates.
        //
        // The length of the name is carried along, so that the comparison
        // does not have to look at the entry: with a million candidates that
        // would be a million random accesses into a table of hundreds of
        // megabytes.

        // The comparison of the order the user asked for.
        //
        // The orders by size and by time need the metadata of the entry, and
        // looking it up in the table of the metadata for **every comparison**
        // is hundreds of millions of lookups with a broad query. So the key
        // of every candidate is worked out once, and the comparison is done
        // on the numbers.
        let by_sort = |a: &(i32, u32, u16), b: &(i32, u32, u16)| -> std::cmp::Ordering {
            match sort {
                FileSort::Relevance => b
                    .0
                    .cmp(&a.0)
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| self.name(a.1).cmp(self.name(b.1))),
                FileSort::Name => self.name(a.1).cmp(self.name(b.1)),
                FileSort::Path => self.path(a.1).cmp(&self.path(b.1)),
                FileSort::Size => {
                    let size = |index: u32| {
                        self.meta
                            .get(&index)
                            .map(|m| m.size)
                            .unwrap_or_else(|| self.entries[index as usize].size)
                    };
                    size(b.1)
                        .cmp(&size(a.1))
                        .then_with(|| self.name(a.1).cmp(self.name(b.1)))
                }
                FileSort::Modified => {
                    let modified = |index: u32| {
                        self.meta
                            .get(&index)
                            .map(|m| m.modified)
                            .unwrap_or_else(|| self.entries[index as usize].modified as u64)
                    };
                    modified(b.1)
                        .cmp(&modified(a.1))
                        .then_with(|| self.name(a.1).cmp(self.name(b.1)))
                }
            }
        };

        // How many candidates are kept while the scan runs.
        //
        // The floor matters: with a small limit the list would be cut down
        // every few candidates, and the cutting down is more work than the
        // scan itself.
        let cap = limit.saturating_mul(4).max(256);
        // Adding one candidate to the best few.
        let keep_best = |mut best: Vec<(i32, u32, u16)>, item: (i32, u32, u16)| {
            best.push(item);
            // The list is cut down when it has grown a few times over the
            // size that is needed. Cutting it down costs a sort, so it is
            // done as rarely as the memory allows.
            if best.len() > cap * 8 {
                best.select_nth_unstable_by(cap, &by_sort);
                best.truncate(cap);
            }
            best
        };
        // Putting two lists of the best few together.
        let merge_best = |mut left: Vec<(i32, u32, u16)>, right: Vec<(i32, u32, u16)>| {
            left.extend(right);
            // The order puts the best first, so the ones before the place
            // that is picked are the best few.
            if left.len() > cap {
                left.select_nth_unstable_by(cap, &by_sort);
            }
            left.truncate(cap);
            left
        };

        // The filter and the evaluation run in one pass, so the list of
        // candidates never exists as a separate allocation.
        //
        // When the order is the relevance, only the best few are kept: the
        // list would otherwise grow with the number of hits, and a broad
        // query has millions of them.
        //
        // The environment variable `LAUNCHER_ALL_CANDIDATES` turns that off,
        // so that the two ways can be measured against each other in one run.
        // 看看有没有新的查询来了。
        //
        // 放在**候选**这一步,而不是每一条:一条要过九百万,候选只有几百万 ——
        // 差三倍,而放弃得一样快(过滤那一遍本来就跑不掉)。
        //
        // 但是「放弃」真的要放弃:以前只是把**那一条**候选扔掉,后面那几千条
        // 照样一个一个去匹配 —— 也就是说,一次被取代的按键仍然把剩下的候选
        // 全部匹配完,再把结果扔掉。现在过期了就记下来,后面的候选一个也不
        // 再匹配。
        let aborted = std::sync::atomic::AtomicBool::new(false);
        let stale = |index: usize| -> bool {
            if index & 1023 != 0 {
                return false;
            }
            let out = match cancel {
                Some((token, generation)) => token.is_stale(generation),
                None => false,
            };
            if out {
                aborted.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            out
        };
        let keep_all = std::env::var_os("LAUNCHER_ALL_CANDIDATES").is_some();
        let mut scored: Vec<(i32, u32, u16)> = {
            let _timer = launcher_core::profile::phase("index.fs.scan");
            match filter {
                Some(mask) if mask != 0 => {
                    let stream = filters
                        .par_iter()
                        .zip(self.excluded.par_iter())
                        .enumerate()
                        // The work is shared out in pieces of a decent size, so
                        // that the number of pieces stays small. Every piece has
                        // a list of the best few of its own, and the lists are put
                        // together at the end.
                        .with_min_len(16_384)
                        .filter(|(_, (entry_mask, excluded))| {
                            !**excluded && **entry_mask & mask == mask
                        })
                        .filter_map(|(index, _)| {
                            if aborted.load(std::sync::atomic::Ordering::Relaxed) {
                                return None;
                            }
                            if stale(index) {
                                return None;
                            }
                            score_of(index as u32)
                        });
                    // Every order keeps only the best few of every piece, so
                    // the list of the candidates never grows with the number of
                    // hits — a broad query has millions of them.
                    if !keep_all {
                        stream
                            .fold(|| Vec::<(i32, u32, u16)>::new(), keep_best)
                            .reduce(|| Vec::<(i32, u32, u16)>::new(), merge_best)
                    } else {
                        stream.collect()
                    }
                }
                _ => {
                    let stream = (0..self.entries.len() as u32)
                        .into_par_iter()
                        .with_min_len(16_384)
                        .filter(|index| !self.excluded[*index as usize])
                        .filter_map(|index| {
                            if aborted.load(std::sync::atomic::Ordering::Relaxed) {
                                return None;
                            }
                            if stale(index as usize) {
                                return None;
                            }
                            score_of(index)
                        });
                    // Every order keeps only the best few of every piece, so
                    // the list of the candidates never grows with the number of
                    // hits — a broad query has millions of them.
                    if !keep_all {
                        stream
                            .fold(|| Vec::<(i32, u32, u16)>::new(), keep_best)
                            .reduce(|| Vec::<(i32, u32, u16)>::new(), merge_best)
                    } else {
                        stream.collect()
                    }
                }
            }
        };

        // ---- keep the best few -----------------------------------------------
        //
        // The list is already short: the scan kept only the best few of
        // every piece, so sorting them is cheap.
        {
            let _timer = launcher_core::profile::phase("index.fs.sort");
            scored.sort_unstable_by(&by_sort);
            scored.truncate(limit);
        }

        // The names and the paths are built for the few that survived — the
        // path is put back together by walking up the tree, which is not
        // cheap, so it is done as late as possible.
        let _timer = launcher_core::profile::phase("index.fs.paths");
        scored
            .into_iter()
            .map(|(score, index, _)| {
                // The size and the time of the file are **in the index**:
                // they were read from the master file table when the table
                // was built, and they are kept in the table on the disk.
                //
                // So they are free: no file has to be opened to know how
                // big it is. The entries that came from the change journal
                // do not carry them, and those are the ones the bitmap says
                // are unknown.
                let known = self.meta_known.get(index as usize).copied().unwrap_or(false);
                let entry = &self.entries[index as usize];
                let (size, modified) = if known {
                    (Some(entry.size), Some(entry.modified as u64))
                } else {
                    (None, None)
                };
                FsHit {
                    name: self.name(index).to_string(),
                    path: self.path(index),
                    is_dir: entry.is_dir(),
                    size,
                    modified,
                    score,
                }
            })
            .collect()
    }
}
