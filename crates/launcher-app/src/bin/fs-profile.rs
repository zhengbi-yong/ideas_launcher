//! Measure the hottest path of the launcher: the search in the full-disk index.
//!
//! The index is the one that was written by an earlier run — nine million
//! entries on a normal machine — so the numbers are the ones the user really
//! feels, and not those of a small table.
//!
//! Run with: `cargo run --release -p ideas-launcher --bin fs-profile`

use std::time::Instant;

use ideas_launcher::paths::cache_directory;
use launcher_core::profile;
use launcher_index::fsindex::FsIndex;

/// The queries a person actually types.
///
/// The short ones are the interesting ones: a single letter matches a large
/// part of the disk, and that is what makes the first keystroke slow.
const QUERIES: &[&str] = &[
    "a", "cl", "cla", "clas", "clash", "note", "report", "chrome", "微信", "xlsx",
];

fn main() {
    profile::set_enabled(true);

    // The launcher keeps the worker pool small, so that a machine with many
    // cores does not wake a thread per core for every query. The scan of the
    // full-disk index is the one thing that wants the whole machine, so the
    // size of the pool is a thing to measure.
    if let Some(threads) = std::env::var("LAUNCHER_RAYON_THREADS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global();
    }

    // The queries can be given on the command line. A profiler that
    // instruments every instruction makes the program ten to a thousand times
    // slower, so a short list is what one wants when the numbers come from
    // such a tool.
    let given: Vec<String> = std::env::args().skip(1).collect();
    // `LAUNCHER_PROFILE_NONE=1` runs the tool without a single query. A tool
    // that counts the instructions needs the difference between "with the
    // query" and "without the query", and the difference is the query.
    let none = std::env::var_os("LAUNCHER_PROFILE_NONE").is_some();
    let queries: Vec<&str> = if none {
        Vec::new()
    } else if given.is_empty() {
        QUERIES.to_vec()
    } else {
        given.iter().map(|q| q.as_str()).collect()
    };
    let rounds = if given.is_empty() { 3 } else { 1 };

    let path = cache_directory().join("fs-index.bin");
    if !path.exists() {
        eprintln!("没有找到索引缓存: {}", path.display());
        eprintln!("先跑一次启动器,让它把全盘索引写下来。");
        std::process::exit(1);
    }

    let started = Instant::now();
    let index = match FsIndex::load(&path) {
        Some(index) => index,
        None => {
            eprintln!("索引缓存读不出来: {}", path.display());
            std::process::exit(1);
        }
    };
    println!(
        "索引读入: {} 条, 用时 {:?}",
        index.len(),
        started.elapsed()
    );
    println!(
        "两张掩码表不一样的条目: {} （0 表示它们是同一份数据）",
        index.mask_divergence()
    );
    println!("卷: {:?}", launcher_index::usn::ntfs_volumes());

    // 取消的效果:扫描跑到一半被打断,要多久才真的停下来。
    //
    // 一次按键要扫全盘,而下一次按键往往很快就来 —— 上一次的结果已经没人要了。
    if let Some(ms) = std::env::var("LAUNCHER_CANCEL_AFTER_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        println!("\n== 取消（{ms} ms 之后叫停）==");
        for query in ["a", "clash"] {
            let cancel = launcher_core::cancel::Cancel::new();
            let generation = cancel.generation();
            let token = cancel.clone();
            let started = Instant::now();
            let handle = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                token.bump();
            });
            let hits = index.search_with_cancel(
                query,
                50,
                None,
                launcher_core::settings::FileSort::default(),
                Some((&cancel, generation)),
            );
            let elapsed = started.elapsed();
            handle.join().ok();
            println!("  {query:<8} 在 {elapsed:?} 停下（{ms} ms 时叫停，命中 {} 条）", hits.len());
        }
    }

    // The first round is the cold one: the pages of the index have to be read
    // from the disk, and that is what the user waits for after a restart.
    if !queries.is_empty() {
        println!("\n== 冷的一轮（刚读进来）==");
        run_round(&index, &queries, 1);

        // The later rounds are the warm ones: the index is in memory.
        println!("\n== 热的一轮 ==");
        run_round(&index, &queries, rounds);
    }

    println!("\n{}", profile::report());
}

/// One round of every query.
fn run_round(index: &FsIndex, queries: &[&str], rounds: usize) {
    for query in queries {
        let mut best = u128::MAX;
        let mut hits = 0usize;
        for _ in 0..rounds {
            let started = Instant::now();
            let found = index.search(query, 50);
            let elapsed = started.elapsed().as_nanos();
            if elapsed < best {
                best = elapsed;
            }
            hits = found.len();
        }
        let candidates = index.candidate_count(query);
        println!(
            "  {query:<10} {hits:>3} 条   候选 {candidates:>9}   最好一次 {best:>8} µs   \
             每候选 {per:>6.1} ns",
            per = if candidates == 0 {
                0.0
            } else {
                best as f64 / candidates as f64
            }
        );
    }
}
