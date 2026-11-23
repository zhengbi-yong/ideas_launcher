//! Measure where the memory goes.
//!
//! A minimal egui window with the same options as the launcher: it shows how
//! much the toolkit and the graphics driver cost on their own, so the rest
//! can be attributed to the index and the plugins.

use eframe::egui::ViewportBuilder;

use ideas_launcher::winapi;

/// The working set and the private memory of this process, in bytes.
fn memory() -> (u64, u64) {
    unsafe {
        let mut counters = winapi::PROCESS_MEMORY_COUNTERS {
            cb: std::mem::size_of::<winapi::PROCESS_MEMORY_COUNTERS>() as u32,
            PageFaultCount: 0,
            PeakWorkingSetSize: 0,
            WorkingSetSize: 0,
            QuotaPeakPagedPoolUsage: 0,
            QuotaPagedPoolUsage: 0,
            QuotaPeakNonPagedPoolUsage: 0,
            QuotaNonPagedPoolUsage: 0,
            PagefileUsage: 0,
            PeakPagefileUsage: 0,
            PrivateUsage: 0,
        };
        let process = winapi::GetCurrentProcess();
        winapi::GetProcessMemoryInfo(process, &mut counters, counters.cb);
        (counters.WorkingSetSize as u64, counters.PrivateUsage as u64)
    }
}

fn report(tag: &str) {
    let (ws, pm) = memory();
    println!("{tag:<28} 工作集 {ws:>10} 字节   私有 {pm:>10} 字节");
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("IDEAS Launcher")
            .with_inner_size([680.0, 460.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_visible(false),
        ..Default::default()
    };

    report("启动前");

    eframe::run_native("IDEAS Launcher", options, Box::new(|cc| {
        report("窗口创建后");

        // Give the driver a moment to settle, then look again.
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            report("三秒后");
            // Keep the window alive.
            ctx.request_repaint();
        });

        Ok(Box::new(Probe))
    }))
}

struct Probe;

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
}
