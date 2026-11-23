//! The service of the IDEAS Launcher.
//!
//! It does the part of the launcher that is worth doing once for the whole
//! machine: it holds the index of every NTFS volume, keeps it up to date,
//! and answers the questions of the launcher through a named pipe.
//!
//! The launcher itself is then a small program. It does not hold nine
//! million file names in its memory, it does not read the disk, and it does
//! not write to it.
//!
//! The switches:
//!
//! * `--install` — register the service with Windows (needs administrator).
//! * `--uninstall` — remove it again.
//! * `--console` — run it in a window instead of as a service (for trying it
//!   out).
//! * `--profile <folder>` — the folder of the user whose files are indexed.

mod server;
mod winapi;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use launcher_core::service::{Request, Response};

use server::Service;
use winapi::*;

/// The service that is running, so that the control handler can stop it.
static RUNNING: OnceLock<Arc<Service>> = OnceLock::new();
/// The state that is reported to the control manager.
static STATE: OnceLock<Arc<ServiceState>> = OnceLock::new();
/// Whether the service was asked to stop.
static STOPPING: AtomicU64 = AtomicU64::new(0);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |name: &str| args.iter().any(|arg| arg == name);
    let profile = profile_from(&args);

    // The log goes to a file: a service has no console, and what it does is
    // worth looking at afterwards.
    let log_path = std::path::PathBuf::from(&profile)
        .join("AppData")
        .join("Roaming")
        .join("IdeasLauncher")
        .join("Logs")
        .join("service.log");
    if let Some(folder) = log_path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();
    let mut builder = env_logger::Builder::new();
    builder.filter_level(log::LevelFilter::Info);
    // Only one target: the last one that is set wins, and a service has no
    // console to write to.
    if let Some(file) = file {
        builder.target(env_logger::fmt::Target::Pipe(Box::new(file)));
    } else {
        builder.target(env_logger::fmt::Target::Stderr);
    }
    builder.init();

    // A panic in a service goes nowhere: there is no console to print it
    // on. The message has to be written down itself, or it is lost.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("the service fell over: {info}");
        default_hook(info);
    }));

    if has("--install") {
        install(&profile);
        return;
    }
    if has("--uninstall") {
        uninstall();
        return;
    }
    if has("--console") {
        run(&profile);
        return;
    }
    if has("--selftest") {
        selftest(&args);
        return;
    }
    if let Some(path) = args
        .iter()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--probe="))
        .map(|path| path.to_string())
    {
        probe(&path);
        return;
    }
    if has("--label-probe") {
        label_probe();
        return;
    }
    if let Some(path) = args
        .iter()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--pipe-probe="))
        .map(|path| path.to_string())
    {
        pipe_probe_to(&path);
        return;
    }
    if has("--pipe-probe") {
        pipe_probe();
        return;
    }
    if has("--rebuild") {
        // Read the table again and write down what changed. It is how the
        // difference between the saved table and the disk is looked at.
        //
        // Nothing is written to the disk: the service that is running is
        // writing the same file, and a diagnostic must not get in its way.
        let service = Service::new(profile);
        service.read_saved();
        service.inspect();
        return;
    }
    // The service control manager started us: hand the table over to it.
    start_as_service(&profile);
}

/// The folder of the user the service serves.
///
/// The control manager does not hand the command line to the service, so it
/// is read from the arguments of the process.
fn profile_from(args: &[String]) -> String {
    args.iter()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--profile="))
        .map(|path| path.to_string())
        .or_else(|| {
            let index = args.iter().position(|arg| arg == "--profile")?;
            args.get(index + 1).cloned()
        })
        .unwrap_or_else(|| "C:\\Users\\Sisyphus".to_string())
}

/// The service control manager wants the table of the entry points.
///
/// This call blocks until the service stops: the control manager calls
/// [`service_main`] when the service starts, and this returns when it is
/// over.
fn start_as_service(_profile: &str) {
    let name: Vec<u16> = wide(server::SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lp_service_name: name.as_ptr(),
            lp_service_proc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lp_service_name: std::ptr::null(),
            lp_service_proc: None,
        },
    ];
    let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if ok == 0 {
        log::error!("the service could not be started: {}", unsafe { GetLastError() });
    }
}

/// The service starts here.
///
/// The control handler has to be registered **before** any state is
/// reported: without it the control manager never hears from the service,
/// and the service stays in `START_PENDING` for ever.
unsafe extern "system" fn service_main(_argc: DWORD, _argv: *mut *mut u16) -> DWORD {
    // The control handler has to be registered **before** any state is
    // reported: without it the control manager never hears from the service.
    // The name is given, because the control manager wants to know which
    // service the handler belongs to.
    let name = wide(server::SERVICE_NAME);
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            name.as_ptr(),
            Some(control_handler),
            std::ptr::null_mut(),
        )
    };
    log::info!("the control handler was registered (handle {handle})");
    STATUS_HANDLE.store(handle, Ordering::Relaxed);

    // A service that dies without telling the control manager leaves its
    // entry stuck in `STOP_PENDING`, and the manager keeps the handle of the
    // dead process — one such leftover per crash. A panic in this build
    // aborts the process at once, so the only chance to say "it is over" is
    // here, before the abort carries the process away.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(state) = STATE.get() {
            state.report(SERVICE_STOPPED, 0);
        }
        previous_hook(info);
    }));

    // The state is reported **before** the work starts: the control manager
    // waits only a moment for it, and a service that says nothing is killed.
    let state = Arc::new(ServiceState::default());
    let _ = STATE.set(state.clone());
    state.set(
        SERVICE_WIN32_OWN_PROCESS,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
    );
    state.report(SERVICE_RUNNING, 0);

    let profile = profile_from(&std::env::args().collect::<Vec<_>>());
    let service = start_work(&profile);
    let _ = RUNNING.set(service.clone());
    // The pipe is served until the service is asked to stop.
    serve(service.clone());
    state.report(SERVICE_STOPPED, 0);
    0
}

/// What the control manager says.
unsafe extern "system" fn control_handler(control: DWORD, _event: u32, _context: *mut std::ffi::c_void, _data: *mut std::ffi::c_void) -> DWORD {
    match control {
        SERVICE_CONTROL_STOP => {
            STOPPING.store(1, Ordering::SeqCst);
            if let Some(state) = STATE.get() {
                state.report(SERVICE_STOP_PENDING, 10_000);
            }
            if let Some(service) = RUNNING.get() {
                service.stop();
            }
            SERVICE_ACCEPT_STOP
        }
        SERVICE_CONTROL_INTERROGATE => {
            if let Some(state) = STATE.get() {
                let current = state.status.lock().unwrap_or_else(|error| error.into_inner());
                let current_state = current.current_state;
                drop(current);
                state.report(current_state, 0);
            }
            SERVICE_ACCEPT_STOP
        }
        _ => 0,
    }
}

/// The work of the service: the index, the journal and the pipe.
fn start_work(profile: &str) -> Arc<Service> {
    let service = Arc::new(Service::new(profile.to_string()));
    let _ = RUNNING.set(service.clone());

    // The saved table first: the service answers questions right away, and
    // the table is brought up to date in the background.
    service.load_saved();
    // The reading of the table allocates and frees a great deal — give the
    // leftovers back before the service settles into its quiet life.
    service.trim_memory();

    // The table carries the place of the change journal with it, so the
    // changes that came after it can be read without touching the disk.
    // This is what makes the service start in a second instead of a minute:
    // the whole disk is only read when there is no table yet.
    if service.catch_up() {
        service.mark_ready();
    } else {
        // The table has to be read from the disk. The journal is followed
        // from the place it stands at right now, so that nothing that
        // happens while the table is being read is lost.
        service.remember_journal_points();
        let builder = service.clone();
        std::thread::Builder::new()
            .name("index-build".into())
            .spawn(move || builder.build())
            .ok();
    }

    let journal = service.clone();
    std::thread::Builder::new()
        .name("journal".into())
        .spawn(move || {
            // The journal waits for the table: while the table is being read,
            // the two would change the same table, and the changes of the
            // journal could be lost when the table that was just read
            // replaces the old one.
            while !journal.is_ready() {
                if journal.should_stop() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            journal.follow_journal();
        })
        .ok();

    // When the change journal is not there, the table is read again once a
    // day — and only the changes are written.
    let refresh = service.clone();
    std::thread::Builder::new()
        .name("refresh".into())
        .spawn(move || refresh.refresh_every_day())
        .ok();

    service
}

/// The work of the service.
fn run(profile: &str) {
    let service = start_work(profile);
    serve(service.clone());
}

/// Serve the pipe until the service is asked to stop.
///
/// One instance of the pipe waits for a client at any time, and the next one
/// is made as soon as a client has come — so a launcher that keeps its
/// connection open does not keep the next one out.
fn serve(service: Arc<Service>) {
    // The privilege that lets the pipe be given an integrity level ordinary
    // programs may write to. Without it a launcher that is started normally
    // could not ask the service anything.
    let privileged = enable_security_privilege();
    if !privileged {
        log::warn!("the privilege to set the integrity level could not be taken");
    }
    // The token of the user who is logged in. The pipe is made with it, so
    // that the launcher may talk to the service even when it is started
    // normally.
    let user = UserToken::new();
    if user.is_none() {
        log::warn!("the token of the user could not be taken");
    }
    // The descriptor holds the DACL of the pipe.
    let name = server::pipe_name();
    let mut failures = 0u32;
    // **How many instances are waiting for a client.**
    let waiting = Arc::new(AtomicUsize::new(0));
    while !service.should_stop() {
        // **Two instances are always waiting.**
        //
        // One is not enough: the launcher opens a second connection right
        // away — it asks the service whether it is there, and then it asks
        // it for the files — and the second connection used to arrive in the
        // moment after the first instance had been taken. There was then no
        // instance left, and Windows answered ERROR_PIPE_BUSY.
        //
        // The launcher took that for "the service is not there", and walked
        // through the folders of the user: more than a million files, three
        // quarters of a minute, while the user was looking at the results.
        while waiting.load(Ordering::SeqCst) < 2 {
            let pipe = match make_pipe(&name, user.as_ref(), &mut failures) {
                Some(pipe) => pipe,
                None => {
                    // The pipe could not be made at all. It is tried again a
                    // moment later; the service keeps running either way.
                    break;
                }
            };
            // The connection waits in a thread of its own: it blocks until a
            // client comes, and the service has to be able to stop while it
            // waits.
            let flag = waiting.clone();
            let client = service.clone();
            // The handle is a raw pointer, which is not `Send`; it is passed
            // as an integer instead.
            let handle = pipe as usize;
            // The counter is raised **before** the thread starts: the thread
            // may take the count down again before this line is done.
            waiting.fetch_add(1, Ordering::SeqCst);
            std::thread::Builder::new()
                .name("pipe-wait".into())
                .spawn(move || {
                    answer_one(handle as HANDLE, &client, flag);
                })
                .ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Try to make the pipe, with and without the token of the user.
///
/// This is what says **where** the pipe goes wrong: the pipe is made twice,
/// once as the service itself and once as the user who is logged in, and
/// what each of them answers is written down.
fn pipe_probe() {
    let lines = pipe_probe_lines();
    for line in &lines {
        println!("{line}");
    }
}

/// The same, but written into a file — a service has no console.
fn pipe_probe_to(path: &str) {
    let lines = pipe_probe_lines();
    let text = lines.join("\n");
    let _ = std::fs::write(path, text);
}

/// What the pipe answers, one line at a time.
fn pipe_probe_lines() -> Vec<String> {
    let mut out = Vec::new();
    let name = server::pipe_name();
    let text: Vec<u16> = name.iter().copied().filter(|c| *c != 0).collect();
    out.push(format!("the pipe: {}", String::from_utf16_lossy(&text)));
    let user = UserToken::new();
    out.push(format!(
        "the token of the user: {}",
        if user.is_some() { "taken" } else { "not taken" }
    ));
    let make = |tag: &str, out: &mut Vec<String>| unsafe {
        let pipe = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            std::ptr::null_mut(),
        );
        let error = GetLastError();
        out.push(format!(
            "  {tag}: handle {}, error {error}",
            if pipe == INVALID_HANDLE_VALUE {
                "none".to_string()
            } else {
                format!("{}", pipe as usize)
            }
        ));
        if pipe != INVALID_HANDLE_VALUE {
            CloseHandle(pipe);
        }
    };
    make("as the service itself", &mut out);
    if let Some(token) = user.as_ref() {
        let ok = token.impersonate();
        out.push(format!("  impersonated: {ok}, error {}", unsafe { GetLastError() }));
        make("as the user", &mut out);
        unsafe { RevertToSelf() };
        make("as the service itself again", &mut out);
    }
    out
}

/// Make one instance of the pipe.
///
/// The pipe is made **as the user who is logged in**, so that a launcher
/// which is started the usual way may write to it: a program of the medium
/// level may not write to an object of a higher level, and the service runs
/// as SYSTEM. Without that the launcher could not ask the service anything
/// at all, and it built an index of its own — a gigabyte of memory instead
/// of a few hundred megabytes.
///
/// `failures` counts how many times in a row it did not work. It is only
/// written down once in a while — a service that tried every second would
/// otherwise fill the log with the same line.
fn make_pipe(
    name: &[u16],
    user: Option<&UserToken>,
    failures: &mut u32,
) -> Option<HANDLE> {
    let make = |descriptor: *mut std::ffi::c_void| unsafe {
        let impersonated = user.map(|token| token.impersonate()).unwrap_or(false);
        // Windows wants the security of the new object in the shape of a
        // structure that holds the descriptor — not the descriptor itself.
        let mut attributes = SECURITY_ATTRIBUTES::with_descriptor(descriptor);
        let pipe = CreateNamedPipeW(
            name.as_ptr(),
            // The pipe is a **blocking** one: the code reads and writes it
            // without an overlapped structure, and a client that opens it the
            // usual way expects the same. With the overlapped flag the
            // connection would have to be waited for in another way, and the
            // calls that come after it would be wrong.
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &mut attributes as *mut SECURITY_ATTRIBUTES,
        );
        if impersonated {
            RevertToSelf();
        }
        pipe
    };
    let descriptor = std::ptr::null_mut();
    let pipe = make(descriptor);
    if pipe != INVALID_HANDLE_VALUE {
        *failures = 0;
        return Some(pipe);
    }
    // The descriptor is something Windows will not take — then the pipe is
    // made without one, with the access list Windows picks by itself. The
    // service keeps running either way; it only asks a little more of the
    // machine that way.
    let error = unsafe { GetLastError() };
    *failures += 1;
    if *failures == 1 || *failures % 60 == 0 {
        log::error!("the pipe could not be opened: {error}");
    }
    let plain = make(std::ptr::null_mut());
    if plain == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        if *failures == 1 || *failures % 60 == 0 {
            log::error!("the plain pipe could not be opened either: {error}");
        }
        return None;
    }
    if *failures == 1 {
        log::warn!("the pipe was made without a descriptor of its own");
    }
    Some(plain)
}

/// Serve one instance of the pipe, until the client goes away.
///
/// The connection waits here, in a thread of its own — the service has to be
/// able to stop even when nobody comes.
fn answer_one(pipe: HANDLE, service: &Arc<Service>, waiting: Arc<AtomicUsize>) {
    let ok = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        if error != ERROR_PIPE_CONNECTED {
            log::warn!("the pipe could not be connected: {error}");
        }
    }
    // The client is there: one instance less is waiting, and another one is
    // made so that there is always a spare.
    waiting.fetch_sub(1, Ordering::SeqCst);
    let client = service.clone();
    // The handle is a raw pointer, which is not `Send`; it is passed as an
    // integer instead.
    let handle = pipe as usize;
    std::thread::Builder::new()
        .name("pipe-client".into())
        .spawn(move || {
            answer_client(handle as HANDLE, &client);
        })
        .ok();
}

/// Answer the requests of one client until it goes away.
fn answer_client(pipe: HANDLE, service: &std::sync::Arc<Service>) {
    loop {
        let Some(request) = read_frame(pipe) else {
            break;
        };
        let response = match request {
            Some(request) => server::answer(service, request),
            None => Response::Error("the request could not be read".to_string()),
        };
        let frame = response.encode();
        let mut written = 0;
        let ok = unsafe {
            WriteFile(
                pipe,
                frame.as_ptr().cast(),
                frame.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            break;
        }
        if service.should_stop() {
            break;
        }
    }
    unsafe {
        DisconnectNamedPipe(pipe);
        CloseHandle(pipe);
    }
}

/// Read one frame from the pipe.
///
/// The pipe is a stream of bytes, so a frame can come in pieces: the header
/// says how long the body is, and the body is read to the end.
fn read_frame(pipe: HANDLE) -> Option<Option<Request>> {
    let mut header = [0u8; 5];
    if !read_exact(pipe, &mut header) {
        return None;
    }
    let length = u32::from_le_bytes(header[1..5].try_into().ok()?) as usize;
    if length > 64 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; length];
    if length > 0 && !read_exact(pipe, &mut body) {
        return None;
    }
    let mut frame = Vec::with_capacity(length + 5);
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&body);
    Some(Request::decode(&frame))
}

/// Read until the buffer is full.
fn read_exact(pipe: HANDLE, buffer: &mut [u8]) -> bool {
    let mut at = 0usize;
    while at < buffer.len() {
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                pipe,
                buffer[at..].as_mut_ptr().cast(),
                (buffer.len() - at) as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return false;
        }
        at += read as usize;
    }
    true
}

/// Ask the service something, and print what it answers.
///
/// This is how the pipe is tried out without a window: the service is
/// started, this is run, and the answers are printed.
fn selftest(args: &[String]) {
    let text = args
        .iter()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--query="))
        .map(|text| text.to_string())
        .or_else(|| {
            let index = args.iter().position(|arg| arg == "--query")?;
            args.get(index + 1).cloned()
        })
        .unwrap_or_else(|| "report".to_string());
    match ask_service(&text) {
        Ok(answer) => println!("{answer}"),
        Err(reason) => {
            log::error!("{reason}");
            println!("{reason}");
        }
    }
}

/// Ask the service something **as an ordinary program**, and write down what
/// happened.
///
/// The point of this mode is that it is run without administrative
/// privileges: the pipe belongs to a service that runs as SYSTEM, and only a
/// launcher of the ordinary kind may talk to it. The answer goes into a file,
/// because a program that is started without a console has nowhere to print.
fn probe(path: &str) {
    let answer = match ask_service("probe") {
        Ok(answer) => format!("管道通得了: {answer}"),
        Err(reason) => format!("管道不通: {reason}"),
    };
    log::info!("{answer}");
    if let Err(error) = std::fs::write(path, answer.as_bytes()) {
        log::error!("the answer could not be written down: {error}");
    }
}

/// Ask the service a question, and say what it answered.
///
/// The answer comes back as a piece of text, or the reason it did not come.
fn ask_service(text: &str) -> Result<String, String> {
    let name: Vec<u16> = wide(launcher_core::service::PIPE_NAME);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        return Err(format!("管道打不开,错误 {error}"));
    }
    let request = launcher_core::service::Request::Query {
        text: text.to_string(),
        limit: 10,
    };
    let frame = request.encode();
    let mut written = 0;
    unsafe {
        WriteFile(
            handle,
            frame.as_ptr().cast(),
            frame.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        );
    }
    let mut header = [0u8; 5];
    if !read_exact(handle, &mut header) {
        unsafe { CloseHandle(handle) };
        return Err("服务没有回答".to_string());
    }
    let length = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
    let mut body = vec![0u8; length];
    read_exact(handle, &mut body);
    let mut whole = header.to_vec();
    whole.extend_from_slice(&body);
    let answer = match launcher_core::service::Response::decode(&whole) {
        Some(launcher_core::service::Response::Hits { hits }) => {
            let mut answer = format!("服务答了 {} 个文件", hits.len());
            for hit in hits.iter().take(10) {
                answer.push_str(&format!("\n  {}  {}", hit.name, hit.path));
            }
            answer
        }
        Some(launcher_core::service::Response::Status(status)) => {
            format!("服务里有 {} 条", status.entries)
        }
        Some(launcher_core::service::Response::Error(text)) => format!("服务说: {text}"),
        None => "答案读不出来".to_string(),
    };
    unsafe { CloseHandle(handle) };
    Ok(answer)
}

/// Make a pipe as the user who is logged in, and see who may talk to it.
///
/// The service runs as SYSTEM, so a pipe it creates carries the integrity
/// level of SYSTEM — and a launcher that is started normally may not write
/// to an object of a higher level. The pipe is therefore made as the user:
/// its level is the medium one, and an ordinary program may talk to it.
fn label_probe() {
    println!("== 特权 ==");
    println!("能拿到 SeSecurityPrivilege: {}", enable_security_privilege());
    // The token of the user who is logged in.
    let user = UserToken::new();
    println!("用户的令牌: {}", user.is_some());
    // One pipe, of its own. Each run gets a name of its own, because an
    // instance that is still there gets in the way of the next one.
    let name_wide = wide(&format!(
        "\\\\.\\pipe\\IdeasLauncherProbe{}",
        std::process::id()
    ));
    let pipe = unsafe {
        let impersonated = user.as_ref().map(|token| token.impersonate()).unwrap_or(false);
        println!("假装成用户: {impersonated}");
        // The pipe is made with the default access list, as the user: the
        // list of the user then decides who may talk to it — and a program
        // of an ordinary kind belongs to that user.
        let mut attributes = SECURITY_ATTRIBUTES::plain();
        let pipe = CreateNamedPipeW(
            name_wide.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &mut attributes as *mut SECURITY_ATTRIBUTES,
        );
        if impersonated {
            RevertToSelf();
        }
        pipe
    };
    if pipe == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        println!("管道开不出来,错误 {error}");
        return;
    }
    println!("管道开得开");
    let client = unsafe {
        CreateFileW(
            name_wide.as_ptr(),
            GENERIC_READ_WRITE,
            0,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if client == INVALID_HANDLE_VALUE {
        let error = unsafe { GetLastError() };
        println!("普通程序打不开: {error}");
    } else {
        println!("普通程序打得开");
        unsafe { CloseHandle(client) };
    }
    unsafe { CloseHandle(pipe) };
}

/// Wait until the service really reports `SERVICE_STOPPED`.
///
/// A stop request is only a request: the process needs a moment to let go of
/// the table, the journal and the pipes. Starting a new one — or deleting the
/// service — **before** the old one is gone is what left dead-but-not-forgotten
/// processes behind: a service killed while the control manager still had it
/// as running never told the manager it was over, and the manager kept the
/// handle of the corpse. So the state is asked for, again and again, until it
/// is really `STOPPED` — or the patience runs out, which is written down.
fn wait_for_stopped(service: HANDLE, seconds: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds as u64);
    loop {
        let mut status = SERVICE_STATUS::default();
        let ok = unsafe { QueryServiceStatus(service, &mut status) };
        if ok == 0 {
            log::warn!(
                "the state of the service could not be asked for: {}",
                unsafe { GetLastError() }
            );
            return false;
        }
        if status.current_state == SERVICE_STOPPED {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            log::warn!(
                "the service is still in state {} after {seconds} s",
                status.current_state
            );
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Register the service with Windows.
fn install(profile: &str) {
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if manager == INVALID_HANDLE_VALUE {
        log::error!("the service manager could not be opened: {}", unsafe { GetLastError() });
        return;
    }
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            log::error!("the path of the program is not known: {error}");
            return;
        }
    };
    let binary = format!("\"{}\" --profile \"{}\"", exe.display(), profile);
    let binary: Vec<u16> = wide(&binary);
    let name = wide(server::SERVICE_NAME);
    let display = wide(server::DISPLAY_NAME);

    // Is it already there? Then it is stopped and started again, so that a
    // new program takes the place of the old one. Installing it a second
    // time is therefore the way to put a new version in.
    let existing = unsafe {
        OpenServiceW(
            manager,
            name.as_ptr(),
            SERVICE_STOP
                | SERVICE_START
                | SERVICE_CHANGE_CONFIG
                | SERVICE_QUERY_STATUS
                | DELETE_ACCESS,
        )
    };
    if existing != INVALID_HANDLE_VALUE {
        log::info!("the service was already there — it is replaced");
        // Stop it, so that the new program takes over. The program on the
        // disk is the new one already: the path does not change. The stop is
        // **waited for**: starting a new process while the old one is still
        // letting go is what used to leave dead processes behind.
        let mut status = SERVICE_STATUS::default();
        unsafe { ControlService(existing, SERVICE_CONTROL_STOP, &mut status) };
        let gone = wait_for_stopped(existing, 20);
        if !gone {
            log::warn!("the old service did not stop in time; it is started anyway");
        }
        let started = unsafe { StartServiceW(existing, 0, std::ptr::null()) };
        if started == 0 {
            log::warn!("the service did not start: {}", unsafe { GetLastError() });
        } else {
            log::info!("the service was replaced and started again");
        }
        unsafe { CloseServiceHandle(existing) };
        unsafe { CloseServiceHandle(manager) };
        return;
    }
    let service = unsafe {
        CreateServiceW(
            manager,
            name.as_ptr(),
            display.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            binary.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if service == INVALID_HANDLE_VALUE {
        log::error!("the service could not be created: {}", unsafe { GetLastError() });
    } else {
        log::info!("the service was installed");
        let started = unsafe { StartServiceW(service, 0, std::ptr::null()) };
        if started == 0 {
            log::warn!("the service did not start: {}", unsafe { GetLastError() });
        }
        unsafe { CloseServiceHandle(service) };
    }
    unsafe { CloseServiceHandle(manager) };
}

/// Remove the service from Windows.
fn uninstall() {
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if manager == INVALID_HANDLE_VALUE {
        log::error!("the service manager could not be opened: {}", unsafe { GetLastError() });
        return;
    }
    let name = wide(server::SERVICE_NAME);
    let service = unsafe { OpenServiceW(manager, name.as_ptr(), SERVICE_STOP | DELETE_ACCESS | SERVICE_CHANGE_CONFIG) };
    if service == INVALID_HANDLE_VALUE {
        log::error!("the service is not there: {}", unsafe { GetLastError() });
    } else {
        let mut status = SERVICE_STATUS {
            service_type: 0,
            current_state: 0,
            controls_accepted: 0,
            win32_exit_code: 0,
            service_specific_exit_code: 0,
            check_point: 0,
            wait_hint: 0,
        };
        unsafe {
            ControlService(service, SERVICE_CONTROL_STOP, &mut status);
        }
        // A service that is deleted while it is still running keeps the
        // control manager waiting for a process that was never told to go —
        // another way the dead ones piled up. The stop is waited for.
        if !wait_for_stopped(service, 20) {
            log::warn!("the service did not stop before it was removed");
        }
        unsafe {
            DeleteService(service);
            CloseServiceHandle(service);
        }
        log::info!("the service was removed");
    }
    unsafe { CloseServiceHandle(manager) };
}
