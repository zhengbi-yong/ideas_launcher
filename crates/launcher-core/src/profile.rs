//! 给启动器的各个阶段计时。
//!
//! 这些数字是用 [`Instant`] 量出来的 —— 是**实测**,不是采样推断。采样只能说
//! "大概有多少时间花在某某地方",计时能说出"这一次花了多久",而且不会因为窗口
//! 本身太小而失真。
//!
//! 计时默认是**关**的:
//!
//! * 开着的时候,每一段都要往一张表里加一条 —— 那是锁和原子操作,在一个空闲时
//!   只占 0.26% CPU 的程序里不是可以随便加的;
//! * 只在想弄明白"哪里慢了"的时候才需要它。
//!
//! 用法:
//!
//! ```ignore
//! let _timer = profile::phase("query.parse");
//! // ...要测的那段活...
//! // 时间在变量离开作用域的时候记下来。
//! ```
//!
//! 或者:
//!
//! ```ignore
//! let hits = profile::measure("query.parse", || parse(text));
//! ```
//!
//! 名字用点分层,报告读起来就是一份**模块级的账单**:
//! `startup.*`、`index.*`、`query.*`、`icon.*`、`render.*`。
//!
//! 测量器是一个可以自己开的 [`Registry`]。程序用的是那一个全局的,想单独
//! 测量什么的人可以再开一个 —— 测量本身不依赖任何全局状态。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What one phase has cost so far.
#[derive(Debug, Clone, Copy)]
pub struct PhaseStat {
    /// How often the phase was measured.
    pub calls: u64,
    /// How long all the calls took together.
    pub total: Duration,
    /// The shortest call.
    pub min: Duration,
    /// The longest call.
    pub max: Duration,
}

impl Default for PhaseStat {
    fn default() -> Self {
        PhaseStat {
            calls: 0,
            total: Duration::ZERO,
            min: Duration::MAX,
            max: Duration::ZERO,
        }
    }
}

impl PhaseStat {
    /// The average of the calls.
    pub fn mean(&self) -> Duration {
        if self.calls == 0 {
            return Duration::ZERO;
        }
        self.total / self.calls as u32
    }
}

/// The measurements of one program.
///
/// The launcher uses the one that [`global`] hands out. Another one can be
/// opened by whatever wants to measure something of its own — the measuring
/// does not lean on any state that is shared.
pub struct Registry {
    /// Whether the phases are measured at all.
    ///
    /// A plain load: the cost of asking is one relaxed read, which is nothing
    /// compared to the work that is being measured.
    enabled: AtomicBool,
    /// The measurements of every phase.
    phases: Mutex<BTreeMap<String, PhaseStat>>,
    /// The counts of every named thing.
    counters: Mutex<BTreeMap<String, u64>>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            enabled: AtomicBool::new(false),
            phases: Mutex::new(BTreeMap::new()),
            counters: Mutex::new(BTreeMap::new()),
        }
    }
}

impl Registry {
    /// Open a measurement of a program of its own.
    pub fn new() -> Self {
        Registry::default()
    }

    /// Turn the measuring on or off.
    ///
    /// Measuring costs something, so it is only worth it while something is
    /// wrong. Off by default.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Whether the phases are measured.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Measure a stretch of time.
    pub fn phase<'r, 'n>(&'r self, name: &'n str) -> Phase<'r, 'n> {
        Phase {
            registry: self,
            name,
            started: if self.enabled() {
                Some(Instant::now())
            } else {
                None
            },
        }
    }

    /// Measure what a piece of work takes.
    pub fn measure<T>(&self, name: &str, work: impl FnOnce() -> T) -> T {
        let _timer = self.phase(name);
        work()
    }

    /// Count something that happened.
    ///
    /// Used for the numbers that go with the times: how many entries were
    /// looked at, how many icons were decoded, how many bytes were read.
    pub fn count(&self, name: &str, n: u64) {
        if !self.enabled() {
            return;
        }
        let mut counts = match self.counters.lock() {
            Ok(counts) => counts,
            Err(poisoned) => poisoned.into_inner(),
        };
        *counts.entry(name.to_string()).or_insert(0) += n;
    }

    /// The measurements, by the total time they took.
    pub fn by_total(&self) -> Vec<(String, PhaseStat)> {
        let table = match self.phases.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut rows: Vec<(String, PhaseStat)> = table.iter().map(|(k, v)| (k.clone(), *v)).collect();
        rows.sort_by(|a, b| b.1.total.cmp(&a.1.total));
        rows
    }

    /// The counts, by name.
    pub fn counts(&self) -> Vec<(String, u64)> {
        let counts = match self.counters.lock() {
            Ok(counts) => counts,
            Err(poisoned) => poisoned.into_inner(),
        };
        counts.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }

    /// Forget every measurement.
    pub fn reset(&self) {
        if let Ok(mut table) = self.phases.lock() {
            table.clear();
        }
        if let Ok(mut counts) = self.counters.lock() {
            counts.clear();
        }
    }

    /// A readable table of the measurements.
    ///
    /// The phases are put in the order of the total time they took, so the
    /// ones that matter are at the top.
    pub fn report(&self) -> String {
        let rows = self.by_total();
        let counts = self.counts();
        if rows.is_empty() && counts.is_empty() {
            return "没有测量到任何东西。\n".to_string();
        }

        let width = rows
            .iter()
            .map(|(name, _)| name.len())
            .chain(counts.iter().map(|(name, _)| name.len()))
            .max()
            .unwrap_or(8)
            .max(8);

        let mut out = String::new();
        out.push_str("== 分阶段耗时 ==\n");
        out.push_str(&format!(
            "{:<width$}  {:>8}  {:>10}  {:>10}  {:>10}  {:>10}\n",
            "阶段",
            "次数",
            "合计",
            "平均",
            "最短",
            "最长",
            width = width
        ));
        for (name, stat) in &rows {
            out.push_str(&format!(
                "{:<width$}  {:>8}  {:>10}  {:>10}  {:>10}  {:>10}\n",
                name,
                stat.calls,
                fmt(stat.total),
                fmt(stat.mean()),
                fmt(stat.min),
                fmt(stat.max),
                width = width
            ));
        }
        if !counts.is_empty() {
            out.push_str("\n== 计数 ==\n");
            for (name, value) in &counts {
                out.push_str(&format!("{name:<width$}  {value:>10}\n", width = width));
            }
        }
        out
    }

    /// Record a measured stretch of time.
    fn record(&self, name: &str, elapsed: Duration) {
        let mut table = match self.phases.lock() {
            Ok(table) => table,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = table.entry(name.to_string()).or_default();
        entry.calls += 1;
        entry.total += elapsed;
        if elapsed < entry.min {
            entry.min = elapsed;
        }
        if elapsed > entry.max {
            entry.max = elapsed;
        }
    }
}

/// A stretch of time that is measured while it lives.
///
/// The time is recorded when the value is dropped, so the measuring covers
/// exactly the stretch of code the value guards.
pub struct Phase<'r, 'n> {
    registry: &'r Registry,
    name: &'n str,
    started: Option<Instant>,
}

impl<'r, 'n> Phase<'r, 'n> {
    /// Stop the measuring and give the time back.
    ///
    /// Dropping the value does the same, so this is only needed when the time
    /// is wanted for something.
    pub fn finish(mut self) -> Duration {
        self.stop()
    }

    /// Record the time, if the measuring is on.
    fn stop(&mut self) -> Duration {
        let started = match self.started.take() {
            Some(started) => started,
            None => return Duration::ZERO,
        };
        let elapsed = started.elapsed();
        if self.registry.enabled() {
            self.registry.record(self.name, elapsed);
        }
        elapsed
    }
}

impl Drop for Phase<'_, '_> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The measurement of the program.
pub fn global() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::new)
}

/// Turn the measuring on or off.
pub fn set_enabled(on: bool) {
    global().set_enabled(on);
}

/// Whether the phases are measured.
pub fn enabled() -> bool {
    global().enabled()
}

/// Measure a stretch of time.
pub fn phase(name: &str) -> Phase<'static, '_> {
    global().phase(name)
}

/// Measure what a piece of work takes.
pub fn measure<T>(name: &str, work: impl FnOnce() -> T) -> T {
    global().measure(name, work)
}

/// Count something that happened.
pub fn count(name: &str, n: u64) {
    global().count(name, n);
}

/// The measurements, by the total time they took.
pub fn by_total() -> Vec<(String, PhaseStat)> {
    global().by_total()
}

/// The counts, by name.
pub fn counts() -> Vec<(String, u64)> {
    global().counts()
}

/// Forget every measurement.
pub fn reset() {
    global().reset();
}

/// A readable table of the measurements.
pub fn report() -> String {
    global().report()
}

/// A duration in a form that is easy to read.
fn fmt(duration: Duration) -> String {
    let micros = duration.as_secs_f64() * 1_000_000.0;
    if micros >= 1000.0 {
        format!("{:.2} ms", micros / 1000.0)
    } else {
        format!("{micros:.1} µs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_a_phase() {
        let registry = Registry::new();
        registry.set_enabled(true);
        let timer = registry.phase("test.phase");
        std::thread::sleep(Duration::from_millis(2));
        let elapsed = timer.finish();
        assert!(elapsed >= Duration::from_millis(1));
        let rows = registry.by_total();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "test.phase");
        assert_eq!(rows[0].1.calls, 1);
        assert!(rows[0].1.total >= Duration::from_millis(1));
    }

    #[test]
    fn counts_things() {
        let registry = Registry::new();
        registry.set_enabled(true);
        registry.count("test.entries", 10);
        registry.count("test.entries", 5);
        let counts = registry.counts();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].1, 15);
    }

    #[test]
    fn measuring_can_be_turned_off() {
        let registry = Registry::new();
        registry.set_enabled(false);
        {
            let _timer = registry.phase("test.off");
        }
        assert!(registry.by_total().is_empty());
    }

    #[test]
    fn writes_a_report() {
        let registry = Registry::new();
        registry.set_enabled(true);
        {
            let _timer = registry.phase("test.report");
        }
        let text = registry.report();
        assert!(text.contains("test.report"));
    }

    #[test]
    fn two_registries_do_not_mix() {
        let one = Registry::new();
        let two = Registry::new();
        one.set_enabled(true);
        two.set_enabled(true);
        {
            let _timer = one.phase("only.one");
        }
        assert_eq!(one.by_total().len(), 1);
        assert!(two.by_total().is_empty());
    }
}
