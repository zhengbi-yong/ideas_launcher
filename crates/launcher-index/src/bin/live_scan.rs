//! 探针：用户目录的实时索引扫到了哪里。
//!
//! 回答三个问题:
//!
//! * 扫了多少条、用时多久、占了多少内存;
//! * 每个根目录（桌面、文档、下载……）各自扫到多少;
//! * 某个具体文件在不在索引里。
//!
//! 用法:`cargo run --release -p launcher-index --bin live_scan -- [想找的路径]`

use std::path::Path;
use std::time::Instant;

use launcher_index::files::{walk, ScanProgress, SearchOptions, Tree};
use launcher_core::settings::FileSort;

fn main() {
    let wanted = std::env::args().nth(1);
    let mut options = SearchOptions::default();
    // The budget can be raised from the outside, to see how much there
    // really is.
    if let Ok(cap) = std::env::var("LIVE_SCAN_CAP") {
        options.max_results = cap.parse().unwrap_or(options.max_results);
    }
    if let Ok(seconds) = std::env::var("LIVE_SCAN_SECONDS") {
        options.timeout = std::time::Duration::from_secs(seconds.parse().unwrap_or(15));
    }
    let started = Instant::now();
    let mut tree = Tree::default();
    walk(&mut tree, &options, &ScanProgress::default());
    let elapsed = started.elapsed();

    // The memory the index takes. The names of all the entries live in one
    // string, and every entry is a fixed number of bytes.
    let names = tree.name_bytes();
    let entries = tree.entry_bytes();
    let bytes = names + entries;
    println!(
        "扫了 {} 条，用时 {:?}，索引约 {} MB（名字 {} MB + 条目 {} MB）",
        tree.len(),
        elapsed,
        bytes / 1024 / 1024,
        names / 1024 / 1024,
        entries / 1024 / 1024,
    );
    println!("上限: {} 条  时限: {:?}", options.max_results, options.timeout);

    // 每个根目录各自贡献了多少条。
    for root in options.roots.iter().chain(options.shallow_roots.iter()) {
        println!("  {:<40} {:>8} 条", root.display(), tree.count_under(root));
    }

    if let Some(wanted) = wanted {
        let path = Path::new(&wanted);
        println!("{} 在索引里: {}", path.display(), tree.has_path(path));
    }

    // 查询要多久。
    for term in ["rustshot", "mp4", "notepad"] {
        let started = Instant::now();
        let hits = tree.search(term, 10_000, FileSort::Relevance, None);
        println!(
            "  查 {term:<10} {:>6} 条  用时 {:?}",
            hits.len(),
            started.elapsed()
        );
    }
}
