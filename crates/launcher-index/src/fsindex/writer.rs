/// A writer that gathers the bytes in a piece and hands them out when the
/// piece is full.
///
/// The index of a big volume is over half a gigabyte, so it is written in
/// pieces: the whole table never has to be in memory a second time.

pub(super) struct ChunkWriter<W: std::io::Write> {
    out: W,
    chunk: Vec<u8>,
}

impl<W: std::io::Write> ChunkWriter<W> {
    /// A writer that gathers a megabyte at a time.
    pub(super) fn new(out: W) -> Self {
        Self {
            out,
            chunk: Vec::with_capacity(1 << 20),
        }
    }

    /// Add bytes to the piece.
    pub(super) fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.chunk.extend_from_slice(bytes);
        if self.chunk.len() >= (1 << 20) {
            self.flush_chunk()?;
        }
        Ok(())
    }

    /// Write the piece out.
    ///
    /// Every full megabyte is followed by a short pause. The index is
    /// written in the background — the change file every now and then, the
    /// whole table when the changes have used it up — and a burst of half a
    /// gigabyte at full speed is exactly the kind of thing that makes
    /// everything else on the disk wait: the disk queue fills up, and
    /// whatever the user is looking at stutters. A pause between the pieces
    /// keeps the queue shallow; the write takes a moment longer and nobody
    /// notices, because it was in the background anyway.
    pub(super) fn flush_chunk(&mut self) -> std::io::Result<()> {
        let big = self.chunk.len() >= (1 << 20);
        if !self.chunk.is_empty() {
            self.out.write_all(&self.chunk)?;
            self.chunk.clear();
        }
        if big {
            std::thread::sleep(std::time::Duration::from_millis(6));
        }
        Ok(())
    }

    /// Write the last piece out.
    pub(super) fn finish(mut self) -> std::io::Result<W> {
        self.flush_chunk()?;
        Ok(self.out)
    }
}

/// The priority the disk gives to the writes of the index.
///
/// `FILE_IO_PRIORITY_HINT` set to `LOW` for the handle of the file: the
/// disk puts these writes **behind** everything else. Together with the
/// pauses between the chunks (see [`ChunkWriter`]), writing the index is
/// something the machine can do in the background without ever getting in
/// the way of the picture on the screen.
#[cfg(target_os = "windows")]
pub(super) mod io_priority {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileInformationByHandle(
            handle: *mut c_void,
            class: u32,
            information: *const c_void,
            size: u32,
        ) -> i32;
    }

    /// The `FileIoPriorityInfo` class.
    const FILE_IO_PRIORITY_INFO: u32 = 9;
    /// `FILE_IO_PRIORITY_HINT_LOW`.
    const LOW: i32 = 1;

    /// Tell the disk that the writes of this file may wait.
    pub fn make_background(file: &std::fs::File) {
        use std::os::windows::io::AsRawHandle;
        let hint: i32 = LOW;
        unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FILE_IO_PRIORITY_INFO,
                &hint as *const i32 as *const c_void,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) mod io_priority {
    pub fn make_background(_file: &std::fs::File) {}
}
