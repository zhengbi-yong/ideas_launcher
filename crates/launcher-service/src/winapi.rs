#![allow(dead_code)]
//! The Win32 calls the service needs.
//!
//! They are declared here by hand, like everywhere else in this project:
//! the launcher has no external dependencies, and the service is part of it.
//!
//! The service needs three things from Windows:
//!
//! * the service control manager, to be started as a service and to be
//!   told when it should stop,
//! * named pipes, to answer the questions of the launcher,
//! * a security descriptor, so that the users of the machine can ask the
//!   service for the list of their files (and nothing else — the service
//!   only ever hands out names, paths and sizes).

use std::ffi::c_void;

pub type BOOL = i32;
pub type HANDLE = *mut c_void;
pub type DWORD = u32;
pub type LPVOID = *mut c_void;
pub type LPCWSTR = *const u16;
/// `LPSECURITY_ATTRIBUTES`
#[allow(non_camel_case_types)]
pub type LPSECURITY_ATTRIBUTES = *mut c_void;

/// `SECURITY_ATTRIBUTES`
///
/// This is the shape Windows wants for the security of a new object: the
/// descriptor itself, and whether the handle is passed on to the children.
/// Handing the descriptor itself to `CreateNamedPipeW` instead of this
/// structure makes the kernel read a pointer out of the middle of it — and
/// the process comes down with an access violation.
///
/// **The size of the structure is the first field, and it has to be filled
/// in.** Without it the kernel reads the fields from the wrong places: the
/// descriptor out of the middle of the structure, and the flag out of thin
/// air behind it. The pipe then comes or goes as it pleases — sometimes it
/// is made, sometimes the call is turned down with a number that means
/// nothing to anyone.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SECURITY_ATTRIBUTES {
    /// The size of this structure.
    pub n_length: DWORD,
    /// The descriptor of the new object.
    pub lp_security_descriptor: *mut c_void,
    /// Whether the handle is passed on to the processes that are started
    /// afterwards.
    pub b_inherit_handle: BOOL,
}

impl SECURITY_ATTRIBUTES {
    /// The attributes of a new object that gets the security Windows picks
    /// by itself.
    pub fn plain() -> Self {
        Self {
            n_length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lp_security_descriptor: std::ptr::null_mut(),
            b_inherit_handle: FALSE,
        }
    }

    /// The attributes of a new object with a descriptor of its own.
    pub fn with_descriptor(descriptor: *mut c_void) -> Self {
        Self {
            n_length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lp_security_descriptor: descriptor,
            b_inherit_handle: FALSE,
        }
    }
}

pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

pub const TRUE: BOOL = 1;
pub const FALSE: BOOL = 0;

// ---- the service control manager -----------------------------------------

/// `SERVICE_WIN32_OWN_PROCESS`
pub const SERVICE_WIN32_OWN_PROCESS: DWORD = 0x0000_0010;
/// `SERVICE_AUTO_START`
pub const SERVICE_AUTO_START: DWORD = 0x0000_0002;
/// `SERVICE_ERROR_NORMAL`
pub const SERVICE_ERROR_NORMAL: DWORD = 0x0000_0001;
/// `SERVICE_CONTROL_STOP`
pub const SERVICE_CONTROL_STOP: DWORD = 0x0000_0001;
/// `SERVICE_CONTROL_INTERROGATE`
pub const SERVICE_CONTROL_INTERROGATE: DWORD = 0x0000_0004;

pub const SERVICE_STOPPED: DWORD = 0x0000_0001;
pub const SERVICE_START_PENDING: DWORD = 0x0000_0002;
pub const SERVICE_STOP_PENDING: DWORD = 0x0000_0003;
pub const SERVICE_RUNNING: DWORD = 0x0000_0004;

pub const SERVICE_ACCEPT_STOP: DWORD = 0x0000_0001;
pub const SERVICE_ACCEPT_SHUTDOWN: DWORD = 0x0000_0004;

/// `SERVICE_STATUS`
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SERVICE_STATUS {
    pub service_type: DWORD,
    pub current_state: DWORD,
    pub controls_accepted: DWORD,
    pub win32_exit_code: DWORD,
    pub service_specific_exit_code: DWORD,
    pub check_point: DWORD,
    pub wait_hint: DWORD,
}

/// `SERVICE_TABLE_ENTRYW`
///
/// The procedure in the table is the **service main** — the function the
/// control manager calls when the service starts — and not the control
/// handler. The control handler is registered from there.
#[repr(C)]
pub struct SERVICE_TABLE_ENTRYW {
    pub lp_service_name: *const u16,
    pub lp_service_proc: Option<
        unsafe extern "system" fn(DWORD, *mut *mut u16) -> u32,
    >,
}

/// The state the service reports to the control manager.
///
/// The state is behind a lock, because it is reported from several threads:
/// the one that starts the service, and the one that hears the control
/// manager. It used to be a plain field, and the two of them worked on
/// **copies** of it — so the service type and the accepted controls never
/// reached the control manager, and the service could not be stopped.
#[derive(Debug, Default)]
pub struct ServiceState {
    pub status: std::sync::Mutex<SERVICE_STATUS>,
}

/// The handle of the service, as the control manager handed it out.
///
/// The state can only be reported with it, and every thread of the service
/// has to be able to report — so it lives here, and not in the state.
pub static STATUS_HANDLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl ServiceState {
    /// Say what kind of service this is, and what it is willing to do.
    pub fn set(&self, service_type: DWORD, controls_accepted: DWORD) {
        let mut status = self.status.lock().unwrap_or_else(|error| error.into_inner());
        status.service_type = service_type;
        status.controls_accepted = controls_accepted;
    }

    /// Report a new state to the control manager.
    ///
    /// The handle is the one that `RegisterServiceCtrlHandlerExW` handed
    /// out: without it the control manager never hears from the service,
    /// and the service stays in `START_PENDING` for ever.
    pub fn report(&self, state: DWORD, wait_hint: DWORD) {
        let mut status = self.status.lock().unwrap_or_else(|error| error.into_inner());
        status.current_state = state;
        status.wait_hint = wait_hint;
        let handle = STATUS_HANDLE.load(std::sync::atomic::Ordering::Relaxed) as HANDLE;
        let ok = unsafe { SetServiceStatus(handle, &*status) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            log::warn!(
                "the state {state} could not be reported (handle {}, error {error})",
                handle as usize
            );
        }
    }
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn StartServiceCtrlDispatcherW(table: *const SERVICE_TABLE_ENTRYW) -> BOOL;
    pub fn RegisterServiceCtrlHandlerExW(
        service_name: LPCWSTR,
        handler: Option<unsafe extern "system" fn(DWORD, u32, *mut c_void, *mut c_void) -> u32>,
        context: *mut c_void,
    ) -> usize;
    pub fn SetServiceStatus(handle: HANDLE, status: *const SERVICE_STATUS) -> BOOL;
    pub fn OpenSCManagerW(machine: LPCWSTR, database: LPCWSTR, access: DWORD) -> HANDLE;
    pub fn CreateServiceW(
        manager: HANDLE,
        service_name: LPCWSTR,
        display_name: LPCWSTR,
        desired_access: DWORD,
        service_type: DWORD,
        start_type: DWORD,
        error_control: DWORD,
        binary_path: LPCWSTR,
        load_order_group: LPCWSTR,
        tag_id: *mut DWORD,
        dependencies: LPCWSTR,
        service_start_name: LPCWSTR,
        service_password: LPCWSTR,
    ) -> HANDLE;
    pub fn DeleteService(service: HANDLE) -> BOOL;
    pub fn CloseServiceHandle(service: HANDLE) -> BOOL;
    pub fn OpenServiceW(
        manager: HANDLE,
        service_name: LPCWSTR,
        desired_access: DWORD,
    ) -> HANDLE;
    pub fn StartServiceW(service: HANDLE, num_service_args: DWORD, service_arg_vectors: *const *const u16) -> BOOL;
    pub fn QueryServiceStatus(service: HANDLE, service_status: *mut SERVICE_STATUS) -> BOOL;
    pub fn ControlService(
        service: HANDLE,
        control: DWORD,
        service_status: *mut SERVICE_STATUS,
    ) -> BOOL;
    pub fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        string_security_descriptor: LPCWSTR,
        string_revision: DWORD,
        security_descriptor: *mut *mut c_void,
        security_descriptor_size: *mut DWORD,
    ) -> BOOL;
    pub fn LocalFree(memory: *mut c_void) -> *mut c_void;
    pub fn SetSecurityDescriptorSacl(
        security_descriptor: *mut c_void,
        sacl_present: BOOL,
        acl: *mut c_void,
        sacl_defaulted: BOOL,
    ) -> BOOL;
    pub fn GetSecurityDescriptorSacl(
        security_descriptor: *mut c_void,
        sacl_present: BOOL,
        acl: *mut *mut c_void,
        acl_size: *mut DWORD,
    ) -> BOOL;
    pub fn MakeSelfRelativeSD(
        absolute: *mut c_void,
        relative: *mut c_void,
        relative_size: *mut DWORD,
    ) -> BOOL;
}

// ---- the integrity level ---------------------------------------------------

/// `TOKEN_ADJUST_PRIVILIGES | TOKEN_QUERY`
pub const TOKEN_ADJUST_PRIVILEGES_QUERY: DWORD = 0x0000_0020 | 0x0000_0008;
/// `SE_PRIVILEGE_ENABLED`
pub const SE_PRIVILEGE_ENABLED: DWORD = 0x0000_0002;
/// `ERROR_SUCCESS`
pub const ERROR_SUCCESS: DWORD = 0;
/// `SecurityImpersonation`
pub const SECURITY_IMPERSONATION: DWORD = 2;
/// `SE_FILE_OBJECT` — the object type of a named pipe.
/// A token of the **user who is logged in**.
///
/// The service runs as SYSTEM, and an object that SYSTEM creates carries the
/// integrity level of SYSTEM with it. A program that is started the usual
/// way runs at the medium level, and it may not write to an object of a
/// higher level — so the launcher, started normally, could not ask the
/// service anything at all. It fell back to building an index of its own,
/// and the whole point of the service was lost.
///
/// So the pipe is made **as the user**: the token of the user who is logged
/// in is of the medium level already, and the pipe inherits it. The service
/// itself keeps running as SYSTEM, and may read and write the pipe all the
/// same — a program of a higher level may write to an object of a lower one.
///
/// (Putting a token *down* to the medium level is refused by Windows — the
/// service tried it, and was told no — but there is no need to: the token of
/// the user is medium already.)
pub struct UserToken {
    token: HANDLE,
}

impl UserToken {
    /// The token of the user who is logged in on this machine.
    ///
    /// Two ways are tried: the token of the session, and the token of a
    /// program of the user. The first one is not always there — the session
    /// of the console may have no token at all (error 1008) — and the second
    /// one always is: the shell of the user runs at the medium level.
    pub fn new() -> Option<UserToken> {
        for session in 1..=32u32 {
            if let Some(token) = Self::of_session(session) {
                log::info!("the token of the user was taken (session {session})");
                return Some(token);
            }
        }
        // The shell of the user: it runs at the medium level, and the pipe
        // takes the level of whoever makes it.
        for name in ["explorer.exe", "dwm.exe", "RuntimeBroker.exe"] {
            if let Some(token) = Self::of_a_process(name) {
                log::info!("the token of {name} was taken");
                return Some(token);
            }
        }
        log::warn!("no token of an ordinary program could be taken");
        None
    }

    /// The token of one session.
    fn of_session(session: u32) -> Option<UserToken> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            // `WTS_CURRENT_SERVER_HANDLE` is 0.
            if WTSQueryUserToken(std::ptr::null_mut(), session, &mut token) == 0 {
                if session <= 3 {
                    log::info!(
                        "the token of the session {session} could not be asked for: {}",
                        GetLastError()
                    );
                }
                return None;
            }
            let copy = Self::copy_of(token);
            CloseHandle(token);
            copy
        }
    }

    /// The token of the first program with that name that is running.
    fn of_a_process(name: &str) -> Option<UserToken> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                log::warn!("the list of the processes could not be made: {}", GetLastError());
                return None;
            }
            let mut entry = PROCESSENTRY32W {
                dw_size: std::mem::size_of::<PROCESSENTRY32W>() as DWORD,
                ..Default::default()
            };
            let mut found = None;
            let mut seen = 0u32;
            let mut listed = 0u32;
            if Process32FirstW(snapshot, &mut entry) != 0 {
                loop {
                    seen += 1;
                    let mut buffer = [0u16; 260];
                    let mut length = 0;
                    for (index, value) in entry.sz_exe_file.iter().enumerate() {
                        if *value == 0 {
                            break;
                        }
                        buffer[index] = *value;
                        length += 1;
                    }
                    let exe = String::from_utf16_lossy(&buffer[..length]);
                    // The name is the whole path; only the last part is of
                    // interest.
                    let file = exe.rsplit('\\').next().unwrap_or(&exe);
                    if listed < 8 {
                        listed += 1;
                        log::info!("  a process: {file}");
                    }
                    if file.eq_ignore_ascii_case(name) {
                        log::info!("{name} was found: the process {}", entry.th32_process_id);
                        let process = OpenProcess(
                            PROCESS_QUERY_LIMITED_INFORMATION,
                            0,
                            entry.th32_process_id,
                        );
                        if process == INVALID_HANDLE_VALUE {
                            log::warn!("the process could not be opened: {}", GetLastError());
                        } else {
                            let mut token: HANDLE = std::ptr::null_mut();
                            let opened = OpenProcessToken(
                                process,
                                TOKEN_OF_A_PROCESS,
                                &mut token,
                            );
                            CloseHandle(process);
                            if opened == 0 {
                                log::warn!("the token of the process could not be opened: {}", GetLastError());
                            } else {
                                let copy = Self::copy_of(token);
                                CloseHandle(token);
                                found = copy;
                                break;
                            }
                        }
                    }
                    if Process32NextW(snapshot, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snapshot);
            if found.is_none() {
                log::info!(
                    "{name} was not among the {seen} processes (the size was {}, the error is {})",
                    entry.dw_size,
                    GetLastError()
                );
            }
            found
        }
    }

    /// A copy of a token, that may be used for pretending to be somebody.
    ///
    /// The copy is of the **impersonating** kind: a thread token has to be
    /// one, and handing a primary token to `SetThreadToken` makes the
    /// kernel read a field that is not there — and the process comes down.
    fn copy_of(token: HANDLE) -> Option<UserToken> {
        unsafe {
            let mut copy: HANDLE = std::ptr::null_mut();
            let copied = DuplicateTokenEx(
                token,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                SECURITY_IMPERSONATION,
                TOKEN_IMPERSONATION,
                &mut copy,
            );
            if copied == 0 {
                log::warn!("the token could not be copied: {}", GetLastError());
                return None;
            }
            Some(UserToken { token: copy })
        }
    }

    /// Pretend to be the user for a moment.
    pub fn impersonate(&self) -> bool {
        unsafe {
            // The way the documentation recommends: it makes the copy it
            // needs itself.
            if ImpersonateLoggedOnUser(self.token) != 0 {
                return true;
            }
            // And the other way, for the case where the first one is refused.
            SetThreadToken(GetCurrentThread(), self.token) != 0
        }
    }
}

impl Drop for UserToken {
    fn drop(&mut self) {
        unsafe {
            RevertToSelf();
            CloseHandle(self.token);
        }
    }
}

/// `LUID`
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LUID {
    pub low_part: DWORD,
    pub high_part: i32,
}

/// `LUID_AND_ATTRIBUTES`
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct LUID_AND_ATTRIBUTES {
    pub luid: LUID,
    pub attributes: DWORD,
}

/// `TOKEN_PRIVILEGES`
///
/// The array has one element in it; Windows wants the privileges to follow
/// the count right after it.
#[repr(C)]
pub struct TOKEN_PRIVILEGES {
    pub privilege_count: DWORD,
    pub privileges: [LUID_AND_ATTRIBUTES; 1],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetCurrentProcess() -> HANDLE;
}

#[link(name = "wtsapi32")]
unsafe extern "system" {
    pub fn WTSQueryUserToken(server: HANDLE, session: DWORD, token: *mut HANDLE) -> BOOL;
    pub fn WTSGetActiveConsoleSessionId() -> DWORD;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn CreateToolhelp32Snapshot(flags: DWORD, process_id: DWORD) -> HANDLE;
    pub fn Process32FirstW(snapshot: HANDLE, entry: *mut PROCESSENTRY32W) -> BOOL;
    pub fn Process32NextW(snapshot: HANDLE, entry: *mut PROCESSENTRY32W) -> BOOL;
    pub fn OpenProcess(access: DWORD, inherit: BOOL, process_id: DWORD) -> HANDLE;
}

/// `TH32CS_SNAPPROCESS`
pub const TH32CS_SNAPPROCESS: DWORD = 0x0000_0002;
/// `PROCESS_QUERY_LIMITED_INFORMATION`
pub const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x0000_1000;
/// `TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_QUERY`
pub const TOKEN_OF_A_PROCESS: DWORD = 0x0001 | 0x0002 | 0x0004 | 0x0008;
/// `TokenImpersonation`
pub const TOKEN_IMPERSONATION: DWORD = 2;

/// `PROCESSENTRY32W`
///
/// The layout is the one of the Windows headers, and it is **not** the one
/// a naive guess gives: `th32DefaultHeapID` is a pointer (`ULONG_PTR`), so it
/// is eight bytes on a 64-bit machine and the name that follows it starts
/// four bytes further on — 564 bytes in total. The kernel checks the size
/// and refuses the entry when it is too small (error 24), and it writes the
/// name where it belongs, so reading it from the wrong place gives pieces
/// of other names.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PROCESSENTRY32W {
    pub dw_size: DWORD,
    pub cnt_usage: DWORD,
    pub th32_process_id: DWORD,
    /// `ULONG_PTR` — eight bytes on a 64-bit machine.
    pub th32_default_heap_id: u64,
    pub th32_module_id: DWORD,
    pub cnt_threads: DWORD,
    pub th32_parent_process_id: DWORD,
    pub pc_pri_class_base: i32,
    pub dw_flags: DWORD,
    /// `szExeFile[MAX_PATH]`
    pub sz_exe_file: [u16; 260],
}

impl Default for PROCESSENTRY32W {
    fn default() -> Self {
        // Every field is a number or an array of numbers, so zero is the
        // value that means "nothing here yet".
        unsafe { std::mem::zeroed() }
    }
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn DuplicateTokenEx(
        source: HANDLE,
        attributes: *mut c_void,
        thread_attributes: *mut c_void,
        impersonation_level: DWORD,
        token_type: DWORD,
        new_token: *mut HANDLE,
    ) -> BOOL;
    pub fn SetTokenInformation(
        token: HANDLE,
        information_class: i32,
        information: *mut c_void,
        length: DWORD,
    ) -> BOOL;
    pub fn ImpersonateLoggedOnUser(token: HANDLE) -> BOOL;
    pub fn SetThreadToken(thread: HANDLE, token: HANDLE) -> BOOL;
    pub fn GetCurrentThread() -> HANDLE;
    pub fn RevertToSelf() -> BOOL;
    pub fn OpenProcessToken(
        process: HANDLE,
        desired_access: DWORD,
        token: *mut HANDLE,
    ) -> BOOL;
    pub fn LookupPrivilegeValueW(system: LPCWSTR, name: LPCWSTR, luid: *mut LUID) -> BOOL;
    pub fn AdjustTokenPrivileges(
        token: HANDLE,
        disable_all_privileges: BOOL,
        new_state: *const TOKEN_PRIVILEGES,
        buffer_length: DWORD,
        previous_state: *mut TOKEN_PRIVILEGES,
        return_length: *mut DWORD,
    ) -> BOOL;
    pub fn SetSecurityInfo(
        handle: HANDLE,
        object_type: i32,
        security_information: DWORD,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> DWORD;
    pub fn GetSecurityInfo(
        handle: HANDLE,
        object_type: i32,
        security_information: DWORD,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> DWORD;
}

    /// Turn on the privileges that the token needs.
    ///
    /// `SeSecurityPrivilege` is the one that has to do with security
    /// descriptors; `SeTcbPrivilege` is the one that has to do with the
    /// operating system itself — and putting a token on a lower level than
    /// the caller's is one of those things.
    ///
    /// The privileges are off by default — even for SYSTEM — so a service
    /// that wants one has to ask for it.
    pub fn enable_security_privilege() -> bool {
        let mut all = true;
        for name in ["SeSecurityPrivilege", "SeTcbPrivilege"] {
            if !enable_one_privilege(name) {
                all = false;
            }
        }
        all
    }

/// Turn on one privilege of the process.
fn enable_one_privilege(name: &str) -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES_QUERY,
            &mut token,
        ) == 0
        {
            return false;
        }
        let text = wide(name);
        let mut luid = LUID::default();
        let found = LookupPrivilegeValueW(std::ptr::null(), text.as_ptr(), &mut luid);
        if found == 0 {
            CloseHandle(token);
            return false;
        }
        let privileges = TOKEN_PRIVILEGES {
            privilege_count: 1,
            privileges: [LUID_AND_ATTRIBUTES {
                luid,
                attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok = AdjustTokenPrivileges(
            token,
            0,
            &privileges as *const TOKEN_PRIVILEGES,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        let error = GetLastError();
        CloseHandle(token);
        ok != 0 && error == ERROR_SUCCESS
    }
}

/// Give an object the **medium** integrity level.
///
/// The service runs as SYSTEM, and an object that SYSTEM creates carries the
/// integrity level of SYSTEM with it. A program that is started the usual way
/// runs at the medium level, and it may not write to an object of a higher
/// level — so the launcher, started normally, could not ask the service
/// anything at all. It fell back to building an index of its own, and the
/// whole point of the service was lost.
///
/// The pipe is therefore given the medium level: ordinary programs may talk
/// to it, and programs that run sandboxed (low level) still may not write to
/// it.
/// Read the integrity level of an object back.
pub const SC_MANAGER_ALL_ACCESS: DWORD = 0x000F_003F;
/// `SC_MANAGER_CONNECT`
pub const SC_MANAGER_CONNECT: DWORD = 0x0000_0001;
/// `SERVICE_ALL_ACCESS`
pub const SERVICE_ALL_ACCESS: DWORD = 0x000F_01FF;
/// `SERVICE_QUERY_STATUS`
pub const SERVICE_QUERY_STATUS: DWORD = 0x0000_0004;
/// `SERVICE_STOP`
pub const SERVICE_STOP: DWORD = 0x0000_0020;
/// `SERVICE_START`
pub const SERVICE_START: DWORD = 0x0000_0010;
/// `DELETE`
pub const DELETE_ACCESS: DWORD = 0x0001_0000;
/// `SERVICE_CHANGE_CONFIG`
pub const SERVICE_CHANGE_CONFIG: DWORD = 0x0000_0002;

// ---- the pipe -------------------------------------------------------------

/// `PIPE_ACCESS_DUPLEX`
pub const PIPE_ACCESS_DUPLEX: DWORD = 0x0000_0003;
/// `FILE_FLAG_OVERLAPPED`
pub const FILE_FLAG_OVERLAPPED: DWORD = 0x4000_0000;
/// `PIPE_TYPE_BYTE`
pub const PIPE_TYPE_BYTE: DWORD = 0x0000_0000;
/// `PIPE_READMODE_BYTE`
pub const PIPE_READMODE_BYTE: DWORD = 0x0000_0000;
/// `PIPE_WAIT`
pub const PIPE_WAIT: DWORD = 0x0000_0000;
/// `PIPE_UNLIMITED_INSTANCES`
pub const PIPE_UNLIMITED_INSTANCES: DWORD = 255;
/// `GENERIC_READ | GENERIC_WRITE`
pub const GENERIC_READ_WRITE: DWORD = 0xC000_0000;
/// `OPEN_EXISTING`
pub const OPEN_EXISTING: DWORD = 3;
/// `ERROR_PIPE_CONNECTED`
pub const ERROR_PIPE_CONNECTED: DWORD = 535;

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetProcessHeap() -> HANDLE;
    pub fn HeapCompact(heap: HANDLE, flags: DWORD) -> BOOL;
    pub fn SetProcessWorkingSetSize(
        process: HANDLE,
        minimum: usize,
        maximum: usize,
    ) -> BOOL;
    pub fn CreateNamedPipeW(
        name: LPCWSTR,
        open_mode: DWORD,
        pipe_mode: DWORD,
        max_instances: DWORD,
        out_buffer_size: DWORD,
        in_buffer_size: DWORD,
        timeout: DWORD,
        security_attributes: *mut SECURITY_ATTRIBUTES,
    ) -> HANDLE;
    pub fn ConnectNamedPipe(pipe: HANDLE, overlapped: *mut c_void) -> BOOL;
    pub fn DisconnectNamedPipe(pipe: HANDLE) -> BOOL;
    pub fn CreateFileW(
        file_name: LPCWSTR,
        desired_access: DWORD,
        share_mode: DWORD,
        security_attributes: LPSECURITY_ATTRIBUTES,
        creation_disposition: DWORD,
        flags_and_attributes: DWORD,
        template_file: HANDLE,
    ) -> HANDLE;
    pub fn ReadFile(
        file: HANDLE,
        buffer: *mut c_void,
        bytes_to_read: DWORD,
        bytes_read: *mut DWORD,
        overlapped: *mut c_void,
    ) -> BOOL;
    pub fn WriteFile(
        file: HANDLE,
        buffer: *const c_void,
        bytes_to_write: DWORD,
        bytes_written: *mut DWORD,
        overlapped: *mut c_void,
    ) -> BOOL;
    pub fn CloseHandle(object: HANDLE) -> BOOL;
    pub fn GetLastError() -> DWORD;
    pub fn WaitNamedPipeW(
        pipe: LPCWSTR,
        timeout: DWORD,
    ) -> BOOL;
}

/// The security descriptor of the pipe.
///
/// The service hands out names, paths and sizes — the same things the
/// launcher itself would show. Every user of the machine may ask for them,
/// and nothing else.
/// A string that lives for as long as the program does.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

