//! Measure the full-disk index.
//!
//! Builds a synthetic index of a configurable size and reports how long the
//! build takes, how long a query takes and how much memory the index uses.
//!
//! ```bash
//! cargo run --release -p launcher-index --bin fs_bench -- 1000000
//! ```

use std::time::{Duration, Instant};

use launcher_index::fsindex::FsIndex;
use launcher_index::usn::{self, UsnEntry};

/// How many queries are timed.
const QUERIES: usize = 200;

/// The queries that are timed.
const TERMS: &[&str] = &[
    "report",
    "setup",
    "config",
    "index",
    "readme",
    "notes",
    "backup",
    "cache",
];

fn main() {
    let target: usize = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(1_000_000);
    let folders = (target / 1000).max(1);

    let started = Instant::now();
    let records = build_records(folders);
    let raw = usn::collect_raw(&records);
    let (names, entries) = raw;
    let names_bytes = names.len();
    let prepared = started.elapsed();

    let volume = usn::Volume {
        root: "C:\\".to_string(),
        device: "\\\\.\\C:".to_string(),
        label: "SYSTEM".to_string(),
    };
    let started = Instant::now();
    let index = FsIndex::build(&[volume], &[(0, entries)], names, &Default::default());
    let build = started.elapsed();

    println!("entries        : {}", index.len());
    println!("prepare        : {prepared:?} (names {names_bytes} bytes)");
    println!("build          : {build:?}");
    println!(
        "per entry      : {} bytes of names",
        names_bytes / index.len().max(1)
    );

    // ---- query latency -----------------------------------------------------
    let mut samples: Vec<Duration> = Vec::with_capacity(QUERIES);
    let mut hits_total = 0usize;
    for round in 0..QUERIES {
        let term = TERMS[round % TERMS.len()];
        let started = Instant::now();
        let hits = index.search(term, 30);
        samples.push(started.elapsed());
        hits_total += hits.len();
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let worst = samples[samples.len() - 1];
    println!("queries        : {QUERIES} (mean {} hits)", hits_total / QUERIES);
    println!("latency p50    : {p50:?}");
    println!("latency p95    : {p95:?}");
    println!("latency worst  : {worst:?}");

    // ---- the cost of the filter --------------------------------------------
    let started = Instant::now();
    let all = index.search("*", 30);
    let wildcard = started.elapsed();
    println!("wildcard query : {wildcard:?} ({} hits)", all.len());
}

/// Build a synthetic table of records.
fn build_records(folders: usize) -> Vec<UsnEntry> {
    let mut records = Vec::with_capacity(folders * 1000 + 1);
    records.push(UsnEntry {
        frn: 5,
        parent_frn: 5,
        name: String::new(),
        attributes: usn::FILE_ATTRIBUTE_DIRECTORY,
    });
    let mut frn = 100u64;
    for folder in 0..folders as u64 {
        let folder_frn = frn;
        frn += 1;
        records.push(UsnEntry {
            frn: folder_frn,
            parent_frn: 5,
            name: format!("folder{folder}"),
            attributes: usn::FILE_ATTRIBUTE_DIRECTORY,
        });
        for file in 0..1000u64 {
            records.push(UsnEntry {
                frn,
                parent_frn: folder_frn,
                name: format!("file{folder}_{file}_report.txt"),
                attributes: 0,
            });
            frn += 1;
        }
    }
    records
}
