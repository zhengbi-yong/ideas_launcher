//! A window that draws nothing, with the same window settings as the launcher.
//!
//! It is the **control** for a question that is hard to answer by reading the
//! code: when the pointer moves over the launcher and the machine feels heavy,
//! is the launcher doing too much — or is the *window itself* expensive on
//! this machine (a 300 Hz panel, an OpenGL surface, and three overlay
//! programs sitting on top of the compositor)?
//!
//! Two windows, the same renderer, the same flags. If the pointer is heavy
//! over this one as well, the launcher is not the problem.

use std::time::{Duration, Instant, SystemTime};

use eframe::egui::ViewportBuilder;

/// How many frames to remember.
const WINDOW: usize = 600;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let on_top = !args.iter().any(|a| a == "--no-top");
    let decorated = args.iter().any(|a| a == "--decorations");
    let changing = args.iter().any(|a| a == "--change");
    // The report is written to a file of its own: a standard output that is
    // redirected to a file is buffered, and the numbers would only show up
    // when the process ends.
    let report = args
        .iter()
        .find(|a| a.starts_with("--report="))
        .map(|a| a["--report=".len()..].to_owned())
        .unwrap_or_else(|| std::env::temp_dir().join("ideas-blank-report.txt").display().to_string());

    let mut viewport = ViewportBuilder::default()
        .with_title("空白对照窗口")
        .with_inner_size([720.0, 330.0])
        .with_decorations(decorated)
        .with_resizable(true)
        .with_visible(true);
    if on_top {
        viewport = viewport.with_always_on_top();
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let _ = eframe::run_native(
        "blank",
        options,
        Box::new(|_cc| Ok(Box::new(Blank {
            report: report.clone(),
            changing,
            ..Blank::default()
        }))),
    );
}

/// The empty application.
struct Blank {
    frames: u32,
    since: Instant,
    /// The time the drawing took, per frame.
    times: Vec<Duration>,
    /// The time between two frames.
    periods: Vec<Duration>,
    last: Instant,
    /// Where the report is written.
    report: String,
    /// Whether the content changes on every frame.
    changing: bool,
    /// The counter that makes the content change.
    tick: u64,
}

impl Default for Blank {
    fn default() -> Self {
        Self {
            frames: 0,
            since: Instant::now(),
            times: Vec::new(),
            periods: Vec::new(),
            last: Instant::now(),
            report: String::new(),
            changing: false,
            tick: 0,
        }
    }
}

impl eframe::App for Blank {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let began = Instant::now();
        // The smallest thing that still asks the window to paint something.
        ui.label("对照窗口");
        let spent = began.elapsed();

        // **The content changes on every frame.** The pixels on the screen are
        // different every time, so the compositor and everything that watches
        // the screen has to take a new frame. The difference between this and
        // a window whose content never changes is the price of a *change*.
        if self.changing {
            self.tick = self.tick.wrapping_add(1);
            ui.label(format!("内容 {}", self.tick));
        }

        // **A click is written down the moment it is seen.** The difference
        // between the moment the click was made and the moment it is written
        // down here is the time the window needed to answer it — and that is
        // what a person feels as "laggy".
        if ui.input(|i| i.pointer.primary_pressed()) {
            let stamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            let line = format!(
                "CLICK {}.{:09} {}",
                stamp.as_secs(),
                stamp.subsec_nanos(),
                began.elapsed().as_nanos()
            );
            println!("{line}");
            if !self.report.is_empty() {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.report)
                {
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }
            }
        }

        self.times.push(spent);
        if self.times.len() > WINDOW {
            self.times.remove(0);
        }
        self.periods.push(began.duration_since(self.last));
        if self.periods.len() > WINDOW {
            self.periods.remove(0);
        }
        self.last = began;
        self.frames += 1;

        let since = self.since.elapsed();
        if since >= Duration::from_secs(1) {
            let mut times = self.times.clone();
            times.sort();
            let mut periods = self.periods.clone();
            periods.sort();
            let pick = |v: &Vec<Duration>, q: f64| {
                if v.is_empty() {
                    Duration::ZERO
                } else {
                    v[((v.len() as f64 - 1.0) * q) as usize]
                }
            };
            let line = format!(
                "fps={:.1}  画一帧 中位 {:.3} ms  p95 {:.3} ms  最大 {:.3} ms  |  帧间隔 中位 {:.2} ms  p95 {:.2} ms",
                self.frames as f64 / since.as_secs_f64(),
                pick(&times, 0.5).as_secs_f64() * 1000.0,
                pick(&times, 0.95).as_secs_f64() * 1000.0,
                times.last().map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0),
                pick(&periods, 0.5).as_secs_f64() * 1000.0,
                pick(&periods, 0.95).as_secs_f64() * 1000.0,
            );
            println!("{line}");
            // The report is written to a file of its own, so that it can be
            // read while the window is still open.
            if !self.report.is_empty() {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.report)
                {
                    let _ = writeln!(file, "{line}");
                    let _ = file.flush();
                }
            }
            self.frames = 0;
            self.since = Instant::now();
        }
    }
}
