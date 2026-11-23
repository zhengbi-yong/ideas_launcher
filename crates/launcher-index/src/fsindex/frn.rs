pub(super) struct FrnMap {
    /// The keys. `u64::MAX` marks an empty slot.
    keys: Vec<u64>,
    /// The values, only meaningful where the key is set.
    values: Vec<u32>,
    /// The number of occupied slots.
    len: usize,
}

impl FrnMap {
    /// A table with room for `expected` entries.
    pub(super) fn with_capacity(expected: usize) -> Self {
        let capacity = (expected * 2).next_power_of_two().max(16);
        Self {
            keys: vec![u64::MAX; capacity],
            values: vec![0; capacity],
            len: 0,
        }
    }

    /// The slot a key belongs in.
    pub(super) fn slot(&self, key: u64) -> usize {
        // A multiply and a shift spread the bits without a table of buckets.
        let hash = key
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .rotate_left(31)
            .wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        ((hash >> 40) as usize) & (self.keys.len() - 1)
    }

    /// Remember `value` for `key`.
    pub(super) fn insert(&mut self, key: u64, value: u32) {
        if (self.len + 1) * 2 > self.keys.len() {
            self.grow();
        }
        let mut slot = self.slot(key);
        while self.keys[slot] != u64::MAX {
            if self.keys[slot] == key {
                self.values[slot] = value;
                return;
            }
            slot = (slot + 1) & (self.keys.len() - 1);
        }
        self.keys[slot] = key;
        self.values[slot] = value;
        self.len += 1;
    }

    /// The value that was stored for `key`.
    pub(super) fn lookup(&self, key: u64) -> Option<&u32> {
        let mut slot = self.slot(key);
        while self.keys[slot] != u64::MAX {
            if self.keys[slot] == key {
                return Some(&self.values[slot]);
            }
            slot = (slot + 1) & (self.keys.len() - 1);
        }
        None
    }

    /// Make the table twice as large.
    pub(super) fn grow(&mut self) {
        let capacity = self.keys.len() * 2;
        let old_keys = std::mem::replace(&mut self.keys, vec![u64::MAX; capacity]);
        let old_values = std::mem::replace(&mut self.values, vec![0; capacity]);
        for (key, value) in old_keys.into_iter().zip(old_values) {
            if key == u64::MAX {
                continue;
            }
            let mut slot = self.slot(key);
            while self.keys[slot] != u64::MAX {
                slot = (slot + 1) & (self.keys.len() - 1);
            }
            self.keys[slot] = key;
            self.values[slot] = value;
        }
    }
}
