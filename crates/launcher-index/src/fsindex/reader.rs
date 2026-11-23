/// A cursor over the bytes of an index file.
///
/// The file is read in pieces, so the whole table never has to be in memory
/// a second time. On a big volume the file is over half a gigabyte.

pub(super) struct FileReader {
    inner: std::io::BufReader<std::fs::File>,
}

impl FileReader {
    /// A reader over a file.
    pub(super) fn new(file: std::fs::File) -> Self {
        Self {
            inner: std::io::BufReader::with_capacity(1 << 20, file),
        }
    }

    /// Fill the buffer with exactly `count` bytes.
    pub(super) fn read_into(&mut self, buffer: &mut [u8]) -> Option<()> {
        use std::io::Read;
        self.inner.read_exact(buffer).ok()
    }

    /// Read a little endian number of `size` bytes.
    pub(super) fn number(&mut self, size: usize) -> Option<u64> {
        let mut buf = [0u8; 8];
        self.read_into(&mut buf[..size])?;
        Some(u64::from_le_bytes(buf))
    }

    /// Read a length prefixed string.
    pub(super) fn text(&mut self) -> Option<String> {
        let length = self.number(2)? as usize;
        let mut buf = vec![0u8; length];
        self.read_into(&mut buf)?;
        Some(String::from_utf8_lossy(&buf).to_string())
    }
}

/// True for a character that the pinyin table covers.
pub(super) fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}
