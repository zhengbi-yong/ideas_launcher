//! Measure the two ways of looking for a substring in a name.
//!
//! **The conclusion is already known: the old way wins.** The new one anchors
//! on the first byte and lets the system find it, which is a block at a time —
//! but the first byte of a query is usually a *common* one (`c` in *clash*,
//! `e` in *report*), so the anchor was found over and over and every try cost
//! two searches over the rest of the name. On the real index that was **30%
//! slower**, and the measurement that settled it is in `analysis/RELEASE-GAP.md`.
//!
//! This benchmark is kept so that the two can be compared again on other data.
//!
//! Run with: `cargo bench --manifest-path perf/Cargo.toml --bench find_fold`

use criterion::{criterion_group, criterion_main, Criterion};

/// The old way: every position of the name is tried, one byte at a time.
fn find_fold_slow(haystack: &str, needle_lower: &str) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    if !needle_lower.is_ascii() {
        return haystack.to_lowercase().find(needle_lower);
    }
    let hay = haystack.as_bytes();
    let ned = needle_lower.as_bytes();
    if ned.len() > hay.len() {
        return None;
    }
    'outer: for start in 0..=(hay.len() - ned.len()) {
        for (offset, expected) in ned.iter().enumerate() {
            if hay[start + offset].to_ascii_lowercase() != *expected {
                continue 'outer;
            }
        }
        return Some(start);
    }
    None
}

/// The new way: the first byte is found by the system's search, the rest is
/// compared.
fn find_fold_fast(haystack: &str, needle_lower: &str) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    if !needle_lower.is_ascii() {
        return haystack.to_lowercase().find(needle_lower);
    }
    let ned = needle_lower.as_bytes();
    if ned.len() > haystack.len() {
        return None;
    }
    let hay = haystack.as_bytes();
    let first = ned[0];
    let upper = first.to_ascii_uppercase();
    let last_start = haystack.len() - ned.len();
    let mut pos = 0usize;
    while pos <= last_start {
        let Some(at) = next_byte(haystack, pos, first, upper) else {
            return None;
        };
        if at > last_start {
            return None;
        }
        if hay[at + 1..at + ned.len()].eq_ignore_ascii_case(&ned[1..]) {
            return Some(at);
        }
        pos = at + 1;
    }
    None
}

fn next_byte(haystack: &str, from: usize, lower: u8, upper: u8) -> Option<usize> {
    let rest = &haystack[from..];
    let a = rest.find(lower as char).map(|at| from + at);
    let b = if upper != lower {
        rest.find(upper as char).map(|at| from + at)
    } else {
        None
    };
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// The names a real disk is made of.
const NAMES: &[&str] = &[
    "Microsoft Visual Studio Code",
    "年度报表 2026 汇总.xlsx",
    "readme",
    "Screenshot 2026-11-07 at 14.35.22.png",
    "clash-verge",
    "notes",
    "Project Euler - Problem 42.md",
    "DESIGN.md",
    "config.json",
    "a",
];

fn find_fold(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_fold");
    group.sample_size(200);
    for needle in ["a", "cl", "cla", "clash", "e", "txt"] {
        group.bench_function(format!("slow-{needle}"), |b| {
            b.iter(|| {
                for name in NAMES {
                    std::hint::black_box(find_fold_slow(std::hint::black_box(name), needle));
                }
            })
        });
        group.bench_function(format!("fast-{needle}"), |b| {
            b.iter(|| {
                for name in NAMES {
                    std::hint::black_box(find_fold_fast(std::hint::black_box(name), needle));
                }
            })
        });
    }
    group.finish();
}

/// The two must answer the same.
#[test]
fn both_find_the_same_place() {
    for name in NAMES {
        for needle in ["a", "cl", "cla", "clash", "e", "txt", "zzz", "aBc"] {
            let lower = needle.to_lowercase();
            assert_eq!(
                find_fold_slow(name, &lower),
                find_fold_fast(name, &lower),
                "{name:?} / {needle:?}"
            );
        }
    }
}

criterion_group!(benches, find_fold);
criterion_main!(benches);
