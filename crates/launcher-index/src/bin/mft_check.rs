//! Hold the reading of the master file table against what the system says.
//!
//! The table is read straight off the disk and every entry is put next to
//! what the system says about the same file: the name, the size and the
//! folder. The report is written to a file as the probe goes, so that an
//! elevated copy can be started with `ShellExecuteEx` and still leave a trace
//! behind, even when it falls over.

use std::io::Write;

use launcher_index::fsindex::FsIndex;
use launcher_index::mft::MftReader;
use launcher_index::usn::{self, Volume};

/// The records that are checked, by their number in the table.
///
/// The first ones are the files of the system. The rest are the folders
/// whose numbers are known from `fsutil file queryfileid`.
fn sample() -> Vec<u64> {
    let mut numbers: Vec<u64> = (0..64).collect();
    numbers.extend([
        0x0e77, // C:\Windows
        0x0e3e, // C:\Users
        0x04b1, // C:\Program Files
        0x0dc0, // C:\ProgramData
        0x15db, // C:\Windows\System32
        0x04ae, // C:\$Recycle.Bin
        5,      // the root of the volume
    ]);
    numbers
}

/// What the table says about one record, next to what the system says.
fn compare(
    volume: &Volume,
    reader: &MftReader,
    number: u64,
    out: &mut dyn Write,
) {
    let _ = writeln!(out, "  --- record {number} ---");
    let _ = out.flush();
    let Some(record) = reader.record(number) else {
        let _ = writeln!(out, "  #{number}: the record could not be read");
        for line in reader.dump(number) {
            let _ = writeln!(out, "{line}");
        }
        let _ = out.flush();
        return;
    };
    let frn = record.file_reference();
    // The name of the folder, as the table says it.
    let parent_name = reader
        .record(record.parent)
        .map(|parent| parent.name)
        .unwrap_or_else(|| String::from("?"));
    let _ = writeln!(
        out,
        "  #{number} seq={} name={:?} size={} parent={:#x} ({:?})",
        record.sequence, record.name, record.size, record.parent, parent_name
    );
    let _ = out.flush();
    let _ = writeln!(out, "  --- identity {number} ---");
    let _ = out.flush();
    // The way that is known to work: open the file by its number and read
    // its attributes.
    let _ = writeln!(out, "  metadata: {:?}", usn::file_metadata(volume, frn));
    let _ = out.flush();
    match usn::file_identity(volume, frn) {
        Some((path, size)) => {
            let _ = writeln!(
                out,
                "  #{number} seq={} name={:?} size={} parent={:#x} ({:?}) | system: size={size} {path}",
                record.sequence, record.name, record.size, record.parent, parent_name
            );
        }
        None => {
            let _ = writeln!(
                out,
                "  #{number} seq={} name={:?} size={} parent={:#x} ({:?}) | system: nothing",
                record.sequence, record.name, record.size, record.parent, parent_name
            );
        }
    }
    let _ = out.flush();
}

/// Write one line to the report.
fn say(out: &mut dyn Write, text: impl AsRef<str>) {
    let _ = writeln!(out, "{}", text.as_ref());
    let _ = out.flush();
}

/// The whole report.
fn run(out: &mut dyn Write) {
    say(out, format!("elevated: {}", usn::is_elevated()));
    let volumes = usn::ntfs_volumes();
    say(out, format!("volumes: {}", volumes.len()));

    for volume in &volumes {
        say(out, format!("==== {} {} ====", volume.device, volume.root));
        let reader = match MftReader::open(volume) {
            Ok(reader) => reader,
            Err(error) => {
                say(out, format!("  could not be opened: {error}"));
                continue;
            }
        };
        say(out, format!("  records: {}", reader.record_count()));
        for line in reader.debug() {
            say(out, line);
        }

        for number in sample() {
            compare(volume, &reader, number, out);
        }

        // A walk over the whole table, to see how long it takes and how much
        // of it is read.
        let started = std::time::Instant::now();
        let mut count = 0u64;
        let mut name_bytes = 0u64;
        let stats = reader.for_each_record(|name, _fields| {
            count += 1;
            name_bytes += name.len() as u64;
        });
        say(
            out,
            format!(
                "  walked {count} records in {:?} (read {} bytes in {} ms, parse {} ms, empty tail {}, extensions {})",
                started.elapsed(),
                stats.bytes,
                stats.read_ms,
                stats.parse_ms,
                stats.empty_tail,
                stats.extensions
            ),
        );
        say(out, format!("  names: {name_bytes} bytes"));
    }

    // The index, built the way the launcher builds it: the entries are
    // handed over one at a time.
    let started = std::time::Instant::now();
    let mut read = 0usize;
    let index = FsIndex::build_from(&volumes, &Default::default(), |sink| {
        for (number, volume) in volumes.iter().enumerate() {
            sink.begin_volume(number as u32, true);
            let _ = launcher_index::usn::enumerate_mft_with(volume, None, |name, fields| {
                sink.add(name, &fields);
                read += 1;
            });
            sink.end_volume();
        }
    });
    say(
        out,
        format!(
            "index built: {} entries in {:?}",
            index.len(),
            started.elapsed()
        ),
    );

    // The index has to survive a round trip through a file, because reading
    // the master file table takes a while and the table is kept between
    // runs.
    let cache = std::env::temp_dir().join("ideas-launcher-mft-check.bin");
    let _ = std::fs::remove_file(&cache);
    let started = std::time::Instant::now();
    match index.save(&cache) {
        Ok(_) => {
            let size = std::fs::metadata(&cache).map(|m| m.len()).unwrap_or(0);
            say(
                out,
                format!("index saved: {size} bytes in {:?}", started.elapsed()),
            );
            let started = std::time::Instant::now();
            match FsIndex::load(&cache) {
                Some(loaded) => say(
                    out,
                    format!(
                        "index loaded back: {} entries in {:?}",
                        loaded.len(),
                        started.elapsed()
                    ),
                ),
                None => say(out, "the index could not be loaded back"),
            }
            for query in [
                "report",
                "*.pdf",
                "folder:",
                "config",
                "everything",
                "README",
                "c:\\windows\\",
            ] {
                // The way it works now: only the best few candidates are
                // kept while the table is scanned.
                let started = std::time::Instant::now();
                let hits = index.search(query, 5);
                say(
                    out,
                    format!(
                        "  query {query:?} -> {} hits in {:?}",
                        hits.len(),
                        started.elapsed()
                    ),
                );
                // The other way: every candidate in one list, sorted at the
                // end. It is measured in the same run, so that the two can be
                // compared without the load of the machine in between.
                unsafe { std::env::set_var("LAUNCHER_ALL_CANDIDATES", "1") };
                let started = std::time::Instant::now();
                let every = index.search(query, 5);
                say(
                    out,
                    format!(
                        "  query {query:?} (all the candidates) -> {} hits in {:?}",
                        every.len(),
                        started.elapsed()
                    ),
                );
                unsafe { std::env::remove_var("LAUNCHER_ALL_CANDIDATES") };
                for hit in hits.iter().take(3) {
                    say(out, format!("      {}  {}", hit.name, hit.path));
                }
            }
            let _ = std::fs::remove_file(&cache);
        }
        Err(error) => say(out, format!("the index could not be saved: {error}")),
    }
}

fn main() {
    let path = std::env::args().nth(1);
    let mut file = match path {
        Some(path) => match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
        {
            Ok(file) => Some(file),
            Err(error) => {
                eprintln!("the report could not be opened: {error}");
                None
            }
        },
        None => None,
    };

    // The release profile aborts on a panic, and an elevated process has no
    // console to write the reason to. The hook puts it into the report
    // instead, so that the place of the fall over is known.
    if let Some(file) = file.as_ref() {
        if let Ok(handle) = file.try_clone() {
            let handle = std::sync::Mutex::new(handle);
            std::panic::set_hook(Box::new(move |info| {
                let mut handle = handle.lock().expect("the report should not be locked");
                let _ = writeln!(handle, "PANIC: {info}");
                if let Some(place) = info.location() {
                    let _ = writeln!(
                        handle,
                        "  at {}:{}:{}",
                        place.file(),
                        place.line(),
                        place.column()
                    );
                }
                let _ = handle.flush();
            }));
        }
    }

    match file.as_mut() {
        Some(file) => run(file),
        None => run(&mut std::io::stdout()),
    }
}
