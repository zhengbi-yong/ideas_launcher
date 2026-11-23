//! Benchmark the search path: index build, cache round-trip and query latency.
//!
//! Run with: `cargo run --release -p ideas-launcher --bin bench`

use std::sync::Arc;
use std::time::Instant;

use ideas_launcher::plugins::{
    bookmarks::BookmarkPlugin, files::FilePlugin, processes::ProcessPlugin, sys::SysPlugin,
    ProgramPlugin,
};
use launcher_core::engine::Engine;
use launcher_core::matcher::{SearchPrecision, StringMatcher};
use launcher_core::plugin::QueryPlugin;
use launcher_core::query::Query;
use launcher_index::files::{FileIndex, SearchOptions};
use launcher_index::{IndexCache, IndexOptions, ProgramIndex};

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let _ = args.next();

    println!("== index build ==");
    let started = Instant::now();
    let index = ProgramIndex::build(&IndexOptions::default());
    let build = started.elapsed();
    println!("programs      : {}", index.programs.len());
    println!("build time    : {build:?}");

    println!("\n== cache round-trip ==");
    let cache_path = std::env::temp_dir().join("ideas-launcher-bench-index.json");
    let cache = IndexCache::new(&cache_path);
    let started = Instant::now();
    cache.save(&index).unwrap();
    let save = started.elapsed();
    let started = Instant::now();
    let loaded = cache.load().expect("cache should load");
    let load = started.elapsed();
    println!("save          : {save:?}");
    println!("load          : {load:?} ({} programs)", loaded.programs.len());
    let _ = std::fs::remove_file(&cache_path);

    println!("\n== query latency ==");
    let matcher = StringMatcher::with_precision(SearchPrecision::Regular);
    let queries = ["chrome", "note", "vs", "code", "x", "term", "s", "pwsh", "a1", "zzzz"];

    // The title alone — what the quick benchmark used to measure.
    let mut title_samples: Vec<u64> = Vec::new();
    // Title *and* the full search text — what the program plugin really does.
    let mut full_samples: Vec<u64> = Vec::new();

    for _ in 0..20 {
        for q in queries {
            let prepared = matcher.prepare(q);

            let started = Instant::now();
            for program in &index.programs {
                let _ = matcher.match_prepared_lower(&prepared, &program.name);
            }
            title_samples.push(started.elapsed().as_nanos() as u64);

            let started = Instant::now();
            let mut hits = 0usize;
            for program in &index.programs {
                let by_title = matcher.match_prepared_lower(&prepared, &program.name);
                let by_text = matcher.match_prepared_lower(&prepared, &program.search_text);
                if by_title.is_some() || by_text.is_some() {
                    hits += 1;
                }
            }
            full_samples.push(started.elapsed().as_nanos() as u64);
            if hits == 0 {
                println!("  {q:<10} no match");
            }
        }
    }
    title_samples.sort_unstable();
    full_samples.sort_unstable();
    println!(
        "title only    : p50 = {} µs   p90 = {} µs   p99 = {} µs   max = {} µs",
        percentile(&title_samples, 0.50) / 1000,
        percentile(&title_samples, 0.90) / 1000,
        percentile(&title_samples, 0.99) / 1000,
        percentile(&title_samples, 1.0) / 1000,
    );
    println!(
        "title + text  : p50 = {} µs   p90 = {} µs   p99 = {} µs   max = {} µs",
        percentile(&full_samples, 0.50) / 1000,
        percentile(&full_samples, 0.90) / 1000,
        percentile(&full_samples, 0.99) / 1000,
        percentile(&full_samples, 1.0) / 1000,
    );
    println!(
        "over {} programs × {} queries × 20 rounds",
        index.programs.len(),
        queries.len()
    );

    println!("\n== per-plugin query cost ==");
    let index = Arc::new(index);
    let program = ProgramPlugin::with_index(index);
    let file_index = Arc::new(FileIndex::new());
    file_index.refresh(SearchOptions::default());
    let files = FilePlugin::new(file_index);
    let bookmarks = BookmarkPlugin::new();
    let sys = SysPlugin::new();
    let processes = ProcessPlugin::new();

    let plugins: Vec<(&str, Arc<dyn QueryPlugin>)> = vec![
        ("program", Arc::new(program)),
        ("file", Arc::new(files)),
        ("bookmark", Arc::new(bookmarks)),
        ("sys", Arc::new(sys)),
        ("process", Arc::new(processes)),
    ];

    for term in ["chrome", "co", "x"] {
        println!("  query {term:?}");
        let query = Query::plain(term);
        for (id, plugin) in &plugins {
            // One warm-up round, then five measured ones.
            let _ = plugin.query(&query);
            let mut samples = Vec::new();
            let mut count = 0usize;
            for _ in 0..5 {
                let started = Instant::now();
                let results = plugin.query(&query).unwrap_or_default();
                samples.push(started.elapsed().as_nanos() as u64);
                count = results.len();
            }
            samples.sort_unstable();
            println!(
                "    {id:<10} p50 = {:>6} µs   max = {:>6} µs   {count} results",
                percentile(&samples, 0.50) / 1000,
                percentile(&samples, 1.0) / 1000,
            );
        }
    }

    println!("\n== engine round trip ==");
    let engine = Engine::new(ideas_launcher::plugins::plugin_context());
    engine.set_max_results(50);
    for (id, plugin) in plugins {
        let _ = id;
        engine.register(plugin);
    }
    let mut samples = Vec::new();
    for _ in 0..20 {
        for term in ["chrome", "co", "x", "note", "vs"] {
            let started = Instant::now();
            let outcome = engine.query(term);
            samples.push(started.elapsed().as_nanos() as u64);
            let _ = outcome;
        }
    }
    samples.sort_unstable();
    println!(
        "engine.query  : p50 = {} µs   p90 = {} µs   p99 = {} µs   max = {} µs",
        percentile(&samples, 0.50) / 1000,
        percentile(&samples, 0.90) / 1000,
        percentile(&samples, 0.99) / 1000,
        percentile(&samples, 1.0) / 1000,
    );
}
