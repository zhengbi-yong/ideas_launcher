//! Probe the NTFS index on this machine.
//!
//! Prints the volumes that can be indexed, enumerates the MFT of each one and
//! reports how long it took and how many entries came back. It is the tool
//! used to check the control codes and the record parser against the real
//! file system.
//!
//! With a file name as the only argument the report is written to that file
//! instead of the console, so that an elevated copy can be started with
//! `ShellExecuteEx` and still leave a trace behind.

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use launcher_index::fsindex::FsIndex;
use launcher_index::usn;

/// Where the report goes.
enum Sink {
    Console,
    File(String),
}

impl Sink {
    fn line(&self, text: &str) {
        match self {
            Sink::Console => println!("{text}"),
            Sink::File(path) => {
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                {
                    let _ = writeln!(file, "{text}");
                }
            }
        }
    }
}

/// The entries of one volume, however they were read.
enum VolumeEntries {
    /// Straight off the disk.
    Table(Vec<(String, usn::RecordFields)>),
    /// Out of the change journal.
    Journal(Vec<usn::RawEntry>),
}

/// The report, written through a sink.
fn report(sink: &Sink) {
    sink.line(&format!(
        "elevated: {}",
        launcher_index::usn::is_elevated()
    ));
    let privilege = launcher_index::usn::enable_manage_volume_privilege_detailed();
    sink.line(&format!(
        "manage-volume privilege: enabled={} stage={} error={} luid={:08x}:{:08x}",
        privilege.enabled,
        privilege.stage,
        privilege.error,
        privilege.luid.1,
        privilege.luid.0
    ));
    sink.line("token privileges:");
    for (name, attributes) in launcher_index::usn::token_privileges() {
        sink.line(&format!("    {name} attributes={attributes:#x}"));
    }
    let raw = launcher_index::usn::token_privilege_bytes();
    sink.line(&format!("raw token privilege buffer: {} bytes", raw.len()));
    for chunk in raw.chunks(16).take(8) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        sink.line(&format!("    {}", hex.join(" ")));
    }
    for name in [
        "SeChangeNotifyPrivilege",
        "SeBackupPrivilege",
        "SeRestorePrivilege",
        "SeManageVolumePrivilege",
        "SeTakeOwnershipPrivilege",
    ] {
        let (enabled, stage, error) = launcher_index::usn::enable_privilege(name);
        sink.line(&format!("    {name}: enabled={enabled} stage={stage} error={error}"));
    }
    let volumes = usn::ntfs_volumes();
    sink.line(&format!("NTFS volumes: {}", volumes.len()));
    for volume in &volumes {
        sink.line(&format!(
            "  {} {} label={:?}",
            volume.device, volume.root, volume.label
        ));
        let journal = usn::journal_state(volume);
        if let Some(journal) = journal {
            sink.line(&format!(
                "    journal id={:#x} first={:#x} next={:#x} max_size={}",
                journal.usn_journal_id,
                journal.first_usn,
                journal.next_usn,
                journal.maximum_size
            ));
        } else {
            sink.line("    journal: unavailable");
        }
    }
    if volumes.is_empty() {
        sink.line("no NTFS volume found");
        return;
    }

    let mut per_volume = Vec::new();
    let mut names = String::new();
    let progress_counter = Arc::new(AtomicU64::new(0));
    for (index, volume) in volumes.iter().enumerate() {
        // The fast path: the table is read straight off the disk.
        let started = Instant::now();
        let report = |count: usize| {
            progress_counter.store(count as u64, Ordering::Relaxed);
        };
        let mut read = Vec::new();
        let mut known = 0usize;
        match usn::enumerate_mft_with(volume, Some(&report), |name, fields| {
            if name.is_empty() {
                return;
            }
            known += 1;
            read.push((name.to_string(), fields));
        }) {
            Ok(_) => {
                let elapsed = started.elapsed();
                let dirs = read.iter().filter(|(_, fields)| fields.is_directory).count();
                sink.line(&format!(
                    "  {} -> {} entries ({} folders, {} files) read straight off the disk in {:?} ({} with a known size)",
                    volume.root,
                    read.len(),
                    dirs,
                    read.len() - dirs,
                    elapsed,
                    known
                ));
                for (name, fields) in read.iter().take(5) {
                    sink.line(&format!(
                        "      frn={:#x} parent={:#x} size={} {}",
                        (fields.sequence as u64) << 48 | fields.number,
                        fields.parent,
                        fields.size,
                        name
                    ));
                }
                per_volume.push((index as u32, VolumeEntries::Table(read)));
                continue;
            }
            Err(error) => sink.line(&format!(
                "  {} could not be read straight off the disk: {error}",
                volume.root
            )),
        }
        // The slow path: the change journal.
        let started = Instant::now();
        match usn::enumerate_volume(volume, &mut names, Some(&report)) {
            Ok(entries) => {
                let elapsed = started.elapsed();
                let dirs = entries.iter().filter(|e| e.is_dir()).count();
                sink.line(&format!(
                    "  {} -> {} entries ({} folders, {} files) through the change journal in {:?}",
                    volume.root,
                    entries.len(),
                    dirs,
                    entries.len() - dirs,
                    elapsed
                ));
                for entry in entries.iter().take(5) {
                    let name =
                        &names[entry.name_start as usize..(entry.name_start + entry.name_len) as usize];
                    sink.line(&format!(
                        "      frn={:#x} parent={:#x} {}",
                        entry.frn, entry.parent_frn, name
                    ));
                }
                per_volume.push((index as u32, VolumeEntries::Journal(entries)));
            }
            Err(error) => {
                sink.line(&format!("  {} -> error: {error}", volume.root));
            }
        }
    }

    if per_volume.is_empty() {
        sink.line("nothing was indexed; the raw control code results follow");
        for line in usn::probe_enumeration(&volumes[0]) {
            sink.line(&line);
        }
        for line in usn::probe_control_codes(&volumes[0]) {
            sink.line(&line);
        }
        return;
    }
    sink.line("the enumeration parameter shapes are a diagnostic; skipping them");

    // The index is built the way the launcher builds it: the entries are
    // handed over one at a time, so the flat list of them never exists.
    let started = Instant::now();
    let index = FsIndex::build_from(&volumes, &Default::default(), |sink| {
        for (volume_id, list) in &per_volume {
            sink.begin_volume(*volume_id, true);
            match list {
                VolumeEntries::Table(rows) => {
                    for (name, fields) in rows {
                        sink.add(name, fields);
                    }
                }
                VolumeEntries::Journal(raw) => {
                    for entry in raw {
                        sink.add_raw(entry);
                    }
                }
            }
            sink.end_volume();
        }
    });
    sink.line(&format!(
        "index built: {} entries in {:?}",
        index.len(),
        started.elapsed()
    ));

    // The index has to survive a round trip through a file, because reading
    // the master file table takes minutes and the table is kept between
    // runs.
    let cache = std::env::temp_dir().join("ideas-launcher-fsindex-probe.bin");
    let _ = std::fs::remove_file(&cache);
    let started = Instant::now();
    match index.save(&cache) {
        Ok(_) => {
            let size = std::fs::metadata(&cache).map(|m| m.len()).unwrap_or(0);
            sink.line(&format!(
                "index saved: {size} bytes in {:?}",
                started.elapsed()
            ));
            let started = Instant::now();
            match FsIndex::load(&cache) {
                Some(loaded) => sink.line(&format!(
                    "index loaded back: {} entries in {:?}",
                    loaded.len(),
                    started.elapsed()
                )),
                None => sink.line("the index could not be loaded back"),
            }
            let _ = std::fs::remove_file(&cache);
        }
        Err(error) => sink.line(&format!("the index could not be saved: {error}")),
    }

    for query in ["report", "*.pdf", "folder:", "config", "everything", "README"] {
        let started = Instant::now();
        let hits = index.search(query, 5);
        sink.line(&format!(
            "  query {query:?} -> {} hits in {:?}",
            hits.len(),
            started.elapsed()
        ));
        for hit in hits.iter().take(3) {
            sink.line(&format!("      {}  {}", hit.name, hit.path));
        }
    }
}

fn main() {
    let sink = match std::env::args().nth(1) {
        Some(path) => {
            let _ = std::fs::remove_file(&path);
            Sink::File(path)
        }
        None => Sink::Console,
    };
    report(&sink);
}
