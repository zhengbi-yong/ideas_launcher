use super::*;

impl FsIndex {
    /// Prepare the index for entries that are added one at a time.
    ///
    /// The full-disk index is read volume by volume, and the entries of a
    /// volume lie next to each other. The index of the user's own folders is
    /// built while the folders are walked, and it all lies on one volume: the
    /// drive the home folder is on.
    pub fn set_single_volume(&mut self, root: &str) {
        self.volumes.clear();
        self.volume_ranges.clear();
        self.volumes.push(IndexedVolume {
            root: root.to_owned(),
            device: String::new(),
            label: String::new(),
        });
        // The range covers every entry, also the ones that are added later.
        self.volume_ranges.push(VolumeRange {
            start: 0,
            end: u32::MAX,
            volume: 0,
        });
    }

    /// Add a file or a folder that was found while the program runs.
    ///
    /// The name goes into the arena and the entry is put at the end of the
    /// tree. The character filters are worked out as well, so that a query
    /// sees the new entry right away.
    pub fn add_live(
        &mut self,
        parent: u32,
        name: &str,
        is_dir: bool,
        size: u64,
        modified: u32,
        known: bool,
    ) -> u32 {
        let name_start = self.names.len() as u32;
        self.names.push_str(name);
        let index = self.entries.len() as u32;
        let depth = if parent == NO_PARENT {
            1
        } else {
            self.entries[parent as usize].depth.saturating_add(1)
        };
        self.entries.push(FsEntry {
            name_start,
            name_len: name.len().min(u16::MAX as usize) as u16,
            parent,
            attributes: if is_dir {
                usn::FILE_ATTRIBUTE_DIRECTORY
            } else {
                0
            },
            size,
            created: 0,
            modified,
            accessed: 0,
            depth,
        });
        self.meta_known.push(known);
        self.frns.push(0);
        self.excluded.push(false);
        // The character filters: the name of the entry, the name of its
        // folder, and the names of all the folders above it.
        let own = self.own_mask_of(index);
        let parent_own = if parent == NO_PARENT {
            0
        } else {
            self.own_mask_of(parent)
        };
        let parent_path = if parent == NO_PARENT {
            0
        } else {
            self.path_masks[parent as usize]
        };
        self.masks.push(own | parent_own);
        self.path_masks.push(parent_path | own);
        index
    }

    /// The character filter of the name of one entry.
    pub(super) fn own_mask_of(&self, index: u32) -> u64 {
        let name = self.name(index);
        let mut mask = name_mask(name);
        if let Some(pinyin) = self.pinyin.get(&index) {
            mask |= name_mask(pinyin);
        }
        mask
    }

    /// Mark an entry as gone.
    ///
    /// The entry stays in the tree — a tree cannot be cut in half — but it is
    /// left out of the results from now on.
    pub fn mark_excluded(&mut self, index: u32) {
        if let Some(slot) = self.excluded.get_mut(index as usize) {
            *slot = true;
        }
    }

    /// Whether an entry is left out.
    pub fn is_excluded(&self, index: u32) -> bool {
        self.excluded.get(index as usize).copied().unwrap_or(false)
    }

    /// The folder of an entry.
    pub fn parent_of(&self, index: u32) -> u32 {
        self.entries
            .get(index as usize)
            .map(|entry| entry.parent)
            .unwrap_or(NO_PARENT)
    }

    /// Change the size and the time of an entry.
    pub fn set_meta_live(&mut self, index: u32, size: u64, modified: u32, known: bool) {
        if let Some(entry) = self.entries.get_mut(index as usize) {
            entry.size = size;
            entry.modified = modified;
        }
        if let Some(flag) = self.meta_known.get_mut(index as usize) {
            *flag = known;
        }
    }
}
