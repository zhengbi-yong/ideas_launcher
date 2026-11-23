//! Benchmark the Win32 program index.
//!
//! Run with: `cargo run --release -p launcher-index --bin index_bench`

use std::time::Instant;

use launcher_index::{IndexOptions, ProgramIndex};

fn main() {
    let options = IndexOptions {
        custom_directories: Vec::new(),
        include_path_env: true,
        include_start_menu: true,
        include_app_paths: true,
        include_uninstall: true,
    };

    println!("building Win32 program index ...");
    let started = Instant::now();
    let index = ProgramIndex::build(&options);
    let elapsed = started.elapsed();

    println!("programs indexed : {}", index.programs.len());
    println!("build time       : {:?}", elapsed);

    let mut by_source = std::collections::BTreeMap::new();
    for p in &index.programs {
        *by_source.entry(p.source.as_str()).or_insert(0usize) += 1;
    }
    println!("by source        :");
    for (source, count) in by_source {
        println!("  {source:<18} {count}");
    }

    // Second run, to show the cost of a cold vs. warm filesystem cache.
    let started = Instant::now();
    let index2 = ProgramIndex::build(&options);
    println!("second build     : {:?}", started.elapsed());
    println!("second programs  : {}", index2.programs.len());
}
