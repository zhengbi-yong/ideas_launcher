//! The launcher executable.

// A launcher is a window program, not a console program. Without this the
// operating system gives the process a console of its own: a black window
// pops up when the launcher starts, that window owns the keyboard focus (so
// the input method never reaches the search box) and everything the program
// logs ends up in there.
//
// The debug build keeps the console, because that is what one wants while
// developing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::ViewportBuilder;
use ideas_launcher::{app, theme};

/// The value of a `--name value` switch.
fn switch_value(args: &[String], name: &str) -> Option<String> {
    let needle = format!("--{name}");
    args.iter()
        .position(|a| a == &needle)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Make sure only one instance of the launcher runs.
///
/// Two instances would fight over the global hotkey: the second one would
/// fail to register it and would look broken.
///
/// When another instance is already running, this one asks it to show its
/// window instead of doing nothing — that is what the user expects when they
/// start the launcher a second time.
fn claim_single_instance(wait: bool) -> bool {
    let id = ideas_launcher::instance::instance_id();
    if wait {
        // The older process is on its way out; give it ten seconds.
        if ideas_launcher::instance::claim_with_wait(&id, 10_000) {
            return true;
        }
    } else if ideas_launcher::instance::claim(&id) {
        return true;
    }
    log::info!("IDEAS Launcher is already running — asking it to show its window");
    if !ideas_launcher::instance::request_show(&id) {
        log::warn!("the running instance did not answer");
    }
    false
}

/// Send the log to a file.
///
/// A window program has no console, so the log would simply disappear.
fn init_logging() {
    let path = ideas_launcher::paths::log_file();
    let _ = std::fs::create_dir_all(
        path.parent()
            .unwrap_or_else(|| std::path::Path::new(".")),
    );

    let file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) => {
            eprintln!("could not open the log file {path:?}: {e}");
            return;
        }
    };

    // The level comes from the settings, so that the noisy parts can be
    // turned on when they are being chased. `RUST_LOG` still wins over it.
    let level = launcher_core::settings::Settings::load(&ideas_launcher::paths::settings_file())
        .log_level
        .clone();
    let level = ideas_launcher::settings_view::log_level_filter(&level);

    env_logger::Builder::new()
        .filter_level(level)
        .parse_env("RUST_LOG")
        .target(env_logger::fmt::Target::Pipe(Box::new(file)))
        .init();
}

/// Say something in a window of the system.
///
/// The launcher has no console, so a message that the user has to see goes
/// into a message box of the system.
fn say(title: &str, text: &str) {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ideas_launcher::winapi::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            ideas_launcher::winapi::MB_OK | ideas_launcher::winapi::MB_ICONINFORMATION,
        );
    }
}

/// Register the service that holds the index of the whole machine.
///
/// The service lives next to the launcher and is started by Windows when the
/// machine starts. From then on the launcher asks it for the files instead
/// of reading the disk itself.
fn install_index_service() {
    let Ok(exe) = std::env::current_exe() else {
        log::warn!("the path of the program is not known");
        return;
    };
    let service = exe
        .parent()
        .map(|folder| folder.join("ideas-launcher-service.exe"));
    let Some(service) = service.filter(|path| path.exists()) else {
        log::warn!("the service program is not next to the launcher");
        say(
            "找不到索引服务程序",
            "这个版本没有把 ideas-launcher-service.exe 放在启动器旁边。",
        );
        return;
    };
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    let status = {
        // **No console window.** The service program is a console program —
        // its probes print to the standard output, which is worth keeping —
        // and Windows gives a console program a console of its own when it
        // is started: a black window that flashes open and shut, once, at
        // every cold start of the launcher. `CREATE_NO_WINDOW` says the
        // standard output is not needed here, and no window appears.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new(&service)
            .arg("--install")
            .arg("--profile")
            .arg(&profile)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
    };
    match status {
        Ok(status) if status.success() => {
            log::info!("the index service was installed");
            say(
                "索引服务已安装",
                "服务会在开机时自己启动,索引由它来管。\n下次启动启动器时它就会用上。",
            );
        }
        Ok(status) => {
            log::warn!("the service could not be installed: {status}");
            say(
                "索引服务没有装上",
                "Windows 拒绝了这次注册。\n不装也不影响使用:启动器会自己管索引。",
            );
        }
        Err(error) => {
            log::warn!("the service could not be started: {error}");
            say(
                "索引服务没有装上",
                "服务程序跑不起来。\n不装也不影响使用:启动器会自己管索引。",
            );
        }
    }
}

fn main() -> eframe::Result<()> {
    init_logging();

    // Keep the worker pool small.
    //
    // Rayon would otherwise start one thread per logical processor — on a
    // 24 core machine that is 24 threads that sit idle almost all of the
    // time. The launcher only needs a handful: six index sources to walk and
    // eight plugins to query.
    //
    // But the search in the full-disk index is the one piece of work that
    // wants the whole machine, and it scales almost to the number of threads
    // (measured on this machine with four million candidates: 423 ms on one
    // thread, 85 ms on four, 50 ms on eight). Four threads left half of it
    // on the table, so the pool is a little bigger — the threads park
    // themselves when there is nothing to do, and the idle cost of a parked
    // thread is nothing.
    if let Err(e) = rayon::ThreadPoolBuilder::new()
        .num_threads(8)
        .build_global()
    {
        log::warn!("could not set the worker pool size: {e}");
    }

    let args: Vec<String> = std::env::args().collect();

    // The measuring of the phases is off by default: it costs a little, and it
    // is only interesting while something is slow. The environment variable
    // turns it on, and the report is written again and again so that it can be
    // read while the program is still running.
    if std::env::var_os("LAUNCHER_PROFILE").is_some() {
        launcher_core::profile::set_enabled(true);
        let report = ideas_launcher::paths::profile_report_file();
        std::thread::Builder::new()
            .name("profile-reporter".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    let text = launcher_core::profile::report();
                    let _ = std::fs::write(&report, text);
                }
            })
            .ok();
    }

    // `--admin` starts a copy with administrative privileges. Reading the
    // NTFS master file table needs them, and that is what gives the launcher
    // its Everything-like full-disk index.
    if args.iter().any(|a| a == "--admin") {
        if ideas_launcher::elevate::relaunch_as_admin() {
            log::info!("an elevated copy was started; this one stops");
            // Give the elevated copy time to come up before the mutex is
            // released.
            std::thread::sleep(std::time::Duration::from_millis(200));
            return Ok(());
        }
        log::warn!("the elevation request was refused");
    }

    // `--install-service` registers the service that holds the index of the
    // whole machine. It needs administrative privileges, so it is only
    // reached through an elevated copy.
    if args.iter().any(|a| a == "--install-service") {
        install_index_service();
        return Ok(());
    }

    // An elevated copy waits for the ordinary process to let go of the
    // single-instance mutex.
    let elevated = args.iter().any(|a| a == "--elevated");
    if !claim_single_instance(elevated) {
        // There is no console to write to any more, so the message goes to
        // the log.
        log::info!("IDEAS Launcher is already running");
        return Ok(());
    }

    // The launcher is supposed to search the whole disk, and that needs
    // administrative privileges: reading the NTFS master file table is what
    // gives the index of every file on the machine. So a copy that does not
    // have them asks for them.
    //
    // The question is asked **in the background**: a launcher has to be there
    // when the user reaches for it, and a window of the system's asking for
    // permission must not keep the launcher from starting. The ordinary copy
    // keeps working (it searches the folders of the user) until the answer
    // comes; when it does, the elevated copy takes over and this one steps
    // aside.
    //
    // The setting can be turned off, and then the launcher stays an ordinary
    // program and searches the folders of the user.
    if !elevated && !args.iter().any(|a| a == "--no-admin") {
        let settings =
            launcher_core::settings::Settings::load(&ideas_launcher::paths::settings_file());
        // The index service holds the index of the whole machine. When it is
        // there, the launcher does **not** need administrative privileges
        // itself: it asks the service, and the service reads the master file
        // table. So it does not ask for them — the launcher stays an ordinary
        // program, and it stays small.
        let service = ideas_launcher::service_client::ServiceClient::new();
        let service_holds_the_index = service.is_there();
        if service_holds_the_index {
            log::info!("the index service is there — no privileges are needed");
        }
        if settings.run_as_administrator && settings.enable_full_disk_index && !service_holds_the_index {
            std::thread::Builder::new()
                .name("elevation".into())
                .spawn(|| {
                    if ideas_launcher::elevate::relaunch_as_admin() {
                        log::info!("an elevated copy was started because the index needs it");
                        // The elevated copy waits for the mutex and for the
                        // hotkey, so this one has to let go of both.
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        std::process::exit(0);
                    }
                    log::warn!("the elevation request was refused; the full-disk index will not be there");
                })
                .ok();
        }
    }

    let started = std::time::Instant::now();
    // `--show` opens the window right away and `--query <text>` fills the
    // search box with it. Both exist so the launcher can be driven without a
    // hotkey, for example when a screenshot is taken.
    let open = args.iter().any(|a| a == "--show");
    let query = switch_value(&args, "query");
    let selftest = switch_value(&args, "selftest");
    // Opens the settings panel instead of the search box. It is useful for
    // looking at the settings without going through the tray menu.
    let settings = args.iter().any(|a| a == "--settings");
    // `--keep-open` leaves the window on the screen even when it does not
    // have the focus. It is for the tools that take a picture of it.
    let keep_open = args.iter().any(|a| a == "--keep-open");

    let options = eframe::NativeOptions {
        // **The window is born far off the screen, and eframe's own memory
        // of where it stood is switched off.** `eframe` shows the window
        // after the very first frame no matter what the builder said
        // (`post_rendering`: "we keep hidden until we've painted something"
        // — a policy for programs that want to be seen). A launcher that
        // lives in the tray would flash onto the screen for a tenth of a
        // second at every start. Born at −32000, the flash happens where
        // nobody can see it, and the window is placed properly the moment
        // it is summoned (`position_at_cursor`).
        viewport: ViewportBuilder::default()
            .with_title("IDEAS Launcher")
            .with_inner_size(theme::metrics::WINDOW_SIZE)
            .with_min_inner_size(theme::metrics::MIN_WINDOW_SIZE)
            .with_position([-32000.0, -32000.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_always_on_top()
            .with_visible(false),
        persist_window: false,
        ..Default::default()
    };

    let result = eframe::run_native("IDEAS Launcher", options, Box::new(|cc| {
        log::info!("the window and the graphics context exist after {:?}", started.elapsed());
        let mut app = app::LauncherApp::new(cc);
        if open {
            app.open_on_startup(query);
        }
        if keep_open {
            app.keep_open();
        }
        if let Some(test) = selftest {
            app.open_on_startup(None);
            app.start_selftest(test);
        }
        if settings {
            app.open_settings();
        }
        log::info!("launcher initialised in {:?}", started.elapsed());
        Ok(Box::new(app))
    }));

    result
}
