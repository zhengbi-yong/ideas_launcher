//! 把 `FSCTL_READ_USN_JOURNAL` 的调用方式全部试一遍,找出内核肯理的那一种。
//!
//! 用法(需要管理员):`usn_journal_probe.exe`
//!
//! 为什么要写这个探针:
//!
//! * 微软自己的 `fsutil usn readjournal` 在同一个提权进程里能读出三十多万条;
//! * 本程序用同一个控制码却总是拿到 `ERROR_INVALID_USER_BUFFER` (1784)。
//!
//! 1784 的意思就是「你给的东西我没法用」。能变的无非是几样:
//!
//! * **请求结构的形状** —— 30 字节(`MaxMajorVersion` 是 `USHORT`)还是 32 字节(`DWORD`);
//! * **版本区间** —— `MinMajorVersion`/`MaxMajorVersion` 是在说「我要这个版本范围的记录」;
//! * **起点** —— 内核也许要求起点落在日志的有效区间里(本机日志从 380 亿开始);
//! * **输入输出是不是同一个缓冲区** —— 记录是写在结构体后面的,同一个缓冲区才对;
//! * **句柄怎么开** —— `CreateFileW` 还是 `NtCreateFile`,原生命名还是 Win32 命名;
//! * **句柄要的权限** —— `fsutil` 用的访问掩码是 `0x100180`。
//!
//! 这些组合一起试一遍,哪个能过就是哪个。

use std::ffi::c_void;
use std::time::Instant;

type RawHandle = *mut c_void;
type Dword = u32;

const INVALID_HANDLE_VALUE: RawHandle = -1isize as RawHandle;
const FILE_SHARE_READ: Dword = 0x0000_0001;
const FILE_SHARE_WRITE: Dword = 0x0000_0002;
const FILE_SHARE_DELETE: Dword = 0x0000_0004;
const OPEN_EXISTING: Dword = 3;
const FILE_FLAG_BACKUP_SEMANTICS: Dword = 0x0200_0000;

const FILE_READ_DATA: Dword = 0x0000_0001;
const FILE_READ_ATTRIBUTES: Dword = 0x0000_0080;
const FILE_WRITE_ATTRIBUTES: Dword = 0x0000_0100;
const SYNCHRONIZE: Dword = 0x0010_0000;

const FSCTL_READ_USN_JOURNAL: Dword = 0x0009_00BB;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: Dword,
        share_mode: Dword,
        security_attributes: *mut c_void,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template_file: RawHandle,
    ) -> RawHandle;
    fn CloseHandle(object: RawHandle) -> i32;
    fn DeviceIoControl(
        device: RawHandle,
        control_code: Dword,
        in_buffer: *const c_void,
        in_buffer_size: Dword,
        out_buffer: *mut c_void,
        out_buffer_size: Dword,
        bytes_returned: *mut Dword,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetLastError() -> Dword;
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: RawHandle,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        file_handle: *mut RawHandle,
        desired_access: Dword,
        object_attributes: *mut ObjectAttributes,
        io_status_placeholder: *mut c_void,
        allocation_size: *mut c_void,
        file_attributes: Dword,
        share_access: Dword,
        create_disposition: Dword,
        create_options: Dword,
        ea_buffer: *mut c_void,
        ea_length: Dword,
    ) -> i32;
}

/// The shape of the request, in the layouts that are in the wild.
#[derive(Clone, Copy, Debug)]
enum Layout {
    /// 30 bytes: `MaxMajorVersion` is a `USHORT`.
    Packed30,
    /// 32 bytes: `MaxMajorVersion` is a `DWORD`.
    Packed32,
}

impl Layout {
    fn build(self, start: u64, min: u32, max: u32) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Layout::Packed30 => {
                out.extend_from_slice(&start.to_le_bytes());
                out.extend_from_slice(&u64::MAX.to_le_bytes()); // HighUsn
                out.extend_from_slice(&0u64.to_le_bytes()); // LowUsn
                out.extend_from_slice(&min.to_le_bytes());
                out.extend_from_slice(&(max as u16).to_le_bytes());
            }
            Layout::Packed32 => {
                out.extend_from_slice(&start.to_le_bytes());
                out.extend_from_slice(&u64::MAX.to_le_bytes());
                out.extend_from_slice(&0u64.to_le_bytes());
                out.extend_from_slice(&min.to_le_bytes());
                out.extend_from_slice(&max.to_le_bytes());
            }
        }
        out
    }

    fn name(self) -> &'static str {
        match self {
            Layout::Packed30 => "30 字节",
            Layout::Packed32 => "32 字节",
        }
    }
}

/// How the buffers are handed to the driver.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// One buffer: the request sits in its head, the records follow it.
    Shared,
    /// Two buffers, as the program does it now.
    Separate,
    /// Two buffers, and the request is written into the head of the output
    /// buffer as well — the kernel may read it from either side.
    SeparateWithHead,
    /// One buffer, and the **whole** buffer is passed as the input as well.
    SharedFull,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Shared => "共用",
            Mode::Separate => "分开",
            Mode::SeparateWithHead => "分开+头部也有",
            Mode::SharedFull => "共用+整个都算输入",
        }
    }
}

/// How the handle is opened.
#[derive(Clone, Copy, Debug)]
enum HandleKind {
    /// `CreateFileW` — the way the program opens it now.
    Win32,
    /// `NtCreateFile` with the native name and the flags `fsutil` uses.
    Native,
}

impl HandleKind {
    fn name(self) -> &'static str {
        match self {
            HandleKind::Win32 => "CreateFileW",
            HandleKind::Native => "NtCreateFile",
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(path: &str, access: Dword) -> Result<RawHandle, Dword> {
    let name = wide(path);
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(unsafe { GetLastError() })
    } else {
        Ok(handle)
    }
}

/// Open the volume the way `fsutil` does it.
///
/// The trace of `fsutil` shows `NtCreateFile` with the native name of the
/// volume, the access mask `0x100180`, the share mode 3, the disposition 1
/// (`FILE_OPEN`) and the options `0x60` — and no `FILE_FLAG_BACKUP_SEMANTICS`.
/// The Win32 layer adds things of its own, and one of them might be what the
/// driver does not want.
fn open_native(path: &str, access: Dword) -> Result<RawHandle, Dword> {
    let mut name = wide(path);
    let mut object = unsafe { std::mem::zeroed::<ObjectAttributes>() };
    object.length = std::mem::size_of::<ObjectAttributes>() as u32;
    object.object_name = &mut UnicodeString {
        length: ((name.len() - 1) * 2) as u16,
        maximum_length: ((name.len() - 1) * 2) as u16,
        buffer: name.as_mut_ptr(),
    };
    object.attributes = 0x40; // OBJ_CASE_INSENSITIVE
    let mut handle: RawHandle = std::ptr::null_mut();
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &mut object,
            std::ptr::null_mut(), // io status
            std::ptr::null_mut(), // allocation size
            0,                    // file attributes
            3,                    // share access
            1,                    // FILE_OPEN
            0x60,                 // create options
            std::ptr::null_mut(), // ea buffer
            0,                    // ea length
        )
    };
    if status != 0 {
        return Err(status as u32);
    }
    Ok(handle)
}

/// One attempt at reading the journal.
fn attempt(
    path: &str,
    access: Dword,
    handle_kind: HandleKind,
    layout: Layout,
    mode: Mode,
    min: u32,
    max: u32,
    start: u64,
) -> Result<(usize, usize, u64), String> {
    let handle = match handle_kind {
        HandleKind::Win32 => open(path, access).map_err(|e| format!("打开失败 {e}"))?,
        HandleKind::Native => open_native(path, access).map_err(|e| format!("打开失败 {e}"))?,
    };
    let request = layout.build(start, min, max);
    let mut buffer = vec![0u8; 1024 * 1024 + 64];
    let mut returned = 0u32;

    let ok = unsafe {
        match mode {
            Mode::Shared => {
                // The request is written into the head of the same buffer that
                // the records are written into.
                buffer[..request.len()].copy_from_slice(&request);
                DeviceIoControl(
                    handle,
                    FSCTL_READ_USN_JOURNAL,
                    buffer.as_mut_ptr().cast(),
                    request.len() as Dword,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as Dword,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            }
            Mode::Separate => DeviceIoControl(
                handle,
                FSCTL_READ_USN_JOURNAL,
                request.as_ptr().cast(),
                request.len() as Dword,
                buffer.as_mut_ptr().cast(),
                buffer.len() as Dword,
                &mut returned,
                std::ptr::null_mut(),
            ),
            Mode::SeparateWithHead => {
                buffer[..request.len()].copy_from_slice(&request);
                DeviceIoControl(
                    handle,
                    FSCTL_READ_USN_JOURNAL,
                    request.as_ptr().cast(),
                    request.len() as Dword,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as Dword,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            }
            Mode::SharedFull => {
                buffer[..request.len()].copy_from_slice(&request);
                DeviceIoControl(
                    handle,
                    FSCTL_READ_USN_JOURNAL,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as Dword,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as Dword,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            }
        }
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        return Err(format!("错误 {}", unsafe { GetLastError() }));
    }

    // The records start right after the request.
    let base = match mode {
        Mode::Shared | Mode::SeparateWithHead | Mode::SharedFull => request.len(),
        Mode::Separate => 0,
    };
    let mut count = 0usize;
    let mut offset = base;
    let mut last = 0u64;
    while offset + 60 <= returned as usize {
        let length = u32::from_le_bytes(buffer[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if length == 0 || offset + length > returned as usize {
            break;
        }
        let usn = i64::from_le_bytes(buffer[offset + 8..offset + 16].try_into().unwrap()) as u64;
        let name_length = u16::from_le_bytes(buffer[offset + 60..offset + 62].try_into().unwrap()) as usize;
        let name_start = offset + 62;
        if name_start + name_length <= buffer.len() {
            let units: Vec<u16> = buffer[name_start..name_start + name_length]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let name = String::from_utf16_lossy(&units);
            if count < 3 {
                println!("      第 {count} 条: usn=0x{usn:x} 名字={name}");
            }
        }
        last = last.max(usn);
        count += 1;
        offset += length;
    }
    Ok((count, returned as usize, last))
}

fn main() {
    println!("== FSCTL_READ_USN_JOURNAL 的调用方式 ==");
    for name in [
        "SeManageVolumePrivilege",
        "SeSystemProfilePrivilege",
        "SeSecurityPrivilege",
    ] {
        let privilege = launcher_index::usn::enable_privilege(name);
        println!("{name}: {}", privilege.0);
    }
    let elevated = launcher_index::usn::is_elevated();
    println!("以管理员身份运行: {elevated}");

    // 先问一句微软自己的工具:它此刻还能不能读?
    //
    // 如果 `fsutil` 此刻也读不动,那就不是本程序调用方式的问题。
    match std::process::Command::new("fsutil")
        .args(["usn", "readjournal", "C:"])
        .output()
    {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = text.lines().take(2).collect();
            println!(
                "fsutil usn readjournal: 退出码 {:?}, 前几行: {:?}",
                output.status.code(),
                lines
            );
        }
        Err(error) => println!("fsutil 跑不起来: {error}"),
    }

    let paths = ["C:\\", "\\??\\C:"];
    let accesses: [(&str, Dword); 3] = [
        ("属性+写属性+同步", FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE),
        ("再要读数据", FILE_READ_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE),
        ("fsutil 的 0x100180", 0x0001_0180),
    ];
    let layouts = [Layout::Packed30, Layout::Packed32];
    let modes = [
        Mode::Shared,
        Mode::Separate,
        Mode::SeparateWithHead,
        Mode::SharedFull,
    ];
    let handles = [HandleKind::Win32, HandleKind::Native];
    let versions = [(0u32, 0u32), (2, 3)];

    // The starting point of the read.
    //
    // The kernel may well insist that the number lies inside the journal:
    // the log on this machine starts at 38 billion, and asking for zero
    // could be what makes it turn the request down.
    let state = launcher_index::usn::ntfs_volumes()
        .first()
        .and_then(|volume| launcher_index::usn::journal_state(volume));
    let (first_usn, next_usn) = match state {
        Some(state) => (state.first_usn as u64, state.next_usn as u64),
        None => (0, 0),
    };
    println!("日志 first=0x{first_usn:x} next=0x{next_usn:x}");
    let starts: [(&str, u64); 3] = [
        ("0", 0),
        ("first", first_usn),
        ("next-1000", next_usn.saturating_sub(1000)),
    ];

    let mut wins = 0usize;
    let mut tried = 0usize;
    for path in paths {
        for (access_name, access) in accesses {
            for handle_kind in handles {
                for layout in layouts {
                    for mode in modes {
                        for (min, max) in versions {
                            for (start_name, start) in starts {
                                tried += 1;
                                let started = Instant::now();
                                match attempt(
                                    path,
                                    access,
                                    handle_kind,
                                    layout,
                                    mode,
                                    min,
                                    max,
                                    start,
                                ) {
                                    Ok((count, bytes, last)) if count > 0 => {
                                        wins += 1;
                                        println!(
                                            "  ✓ {} | {} | {} | {} | {} | 版本 {min}..{max} | 起点 {start_name} -> {count} 条, {bytes} 字节, 最后 usn=0x{last:x} ({:?})",
                                            path,
                                            access_name,
                                            handle_kind.name(),
                                            layout.name(),
                                            mode.name(),
                                            started.elapsed()
                                        );
                                    }
                                    Ok(_) => {
                                        println!(
                                            "  ~ {} | {} | {} | {} | {} | 版本 {min}..{max} | 起点 {start_name} -> 成功但没有记录 ({:?})",
                                            path,
                                            access_name,
                                            handle_kind.name(),
                                            layout.name(),
                                            mode.name(),
                                            started.elapsed()
                                        );
                                    }
                                    Err(reason) => {
                                        if reason == "错误 1784" {
                                            println!(
                                                "  ✗ {} | {} | {} | {} | {} | 版本 {min}..{max} | 起点 {start_name} -> {}",
                                                path,
                                                access_name,
                                                handle_kind.name(),
                                                layout.name(),
                                                mode.name(),
                                                reason
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("试了 {tried} 种,成了 {wins} 种");
}
