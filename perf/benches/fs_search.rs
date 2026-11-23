//! Lock the numbers of the search in the full-disk index.
//!
//! The index is the one that an earlier run wrote — nine million entries on a
//! normal machine — so the numbers are the ones the user really feels.
//!
//! Run with: `cargo bench --manifest-path perf/Cargo.toml`

use std::path::PathBuf;

use criterion::{criterion_group, criterion_main, Criterion};
use launcher_index::fsindex::FsIndex;

/// The queries a person actually types.
const QUERIES: &[&str] = &["clash", "report", "chrome", "xlsx", "a", "微信"];

/// Where the index is kept between runs.
fn index_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("IdeasLauncher")
        .join("Cache")
        .join("fs-index.bin")
}

fn fs_search(c: &mut Criterion) {
    // The same worker pool the launcher uses, so that the numbers are the
    // ones the program really produces.
    let _ = rayon::ThreadPoolBuilder::new().num_threads(8).build_global();

    let path = index_path();
    if !path.exists() {
        eprintln!("没有找到索引缓存: {}", path.display());
        eprintln!("先跑一次启动器,让它把全盘索引写下来。");
        return;
    }
    let index = FsIndex::load(&path).expect("索引缓存读不出来");
    eprintln!("索引: {} 条", index.len());

    let mut group = c.benchmark_group("fs_search");
    // The queries are slow enough that a hundred samples would take minutes.
    group.sample_size(20);
    group.measurement_time(std::time::Duration::from_secs(20));
    for query in QUERIES {
        group.bench_function(*query, |b| b.iter(|| index.search(query, 50)));
    }
    group.finish();
}

criterion_group!(benches, fs_search);
criterion_main!(benches);
