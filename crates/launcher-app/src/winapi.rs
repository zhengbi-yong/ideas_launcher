//! Minimal Win32 declarations.
//!
//! The launcher needs a handful of Win32 entry points. Declaring them here
//! keeps the dependency surface small and avoids pulling in a large bindings
//! crate for a few stable ABI calls.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub type BOOL = i32;
pub type HANDLE = *mut c_void;
pub type HWND = *mut c_void;
pub type HICON = *mut c_void;
pub type HCURSOR = *mut c_void;
pub type HBITMAP = *mut c_void;
pub type HDC = *mut c_void;
pub type HGDIOBJ = *mut c_void;
pub type HINSTANCE = *mut c_void;
pub type HBRUSH = *mut c_void;
pub type HKEY = *mut c_void;
pub type HMENU = *mut c_void;
pub type UINT = u32;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type DWORD = u32;
pub type BOOL_PTR = *mut BOOL;
/// A window procedure.
pub type WNDPROC = Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>;

/// One raw input device.
#[repr(C)]
pub struct RAWINPUTDEVICE {
    /// The page of the usage: 1 is the mouse.
    pub usUsagePage: u16,
    /// The usage: 2 is the mouse.
    pub usUsage: u16,
    /// What to do with the registration.
    pub dwFlags: u32,
    /// The window that gets the input.
    pub hwndTarget: HWND,
}

/// Take the registration of a raw input device away.
pub const RIDEV_REMOVE: u32 = 0x0000_0001;
/// Do not send the legacy messages as well.
pub const RIDEV_NOLEGACY: u32 = 0x0000_0004;
/// Send the input even when the window is not in front.
pub const RIDEV_INPUTSINK: u32 = 0x0000_0100;

pub const TRUE: BOOL = 1;
pub const FALSE: BOOL = 0;

pub const SW_HIDE: i32 = 0;
pub const SW_SHOW: i32 = 5;

pub const WM_HOTKEY: UINT = 0x0312;
pub const WM_APP_WAKE: UINT = 0x8001;
/// The pointer moved.
pub const WM_MOUSEMOVE: UINT = 0x0200;
/// The wheel of the mouse was turned.
pub const WM_MOUSEWHEEL: UINT = 0x020A;
/// A timer that was started with `SetTimer` ran out.
pub const WM_TIMER: UINT = 0x0113;
/// The index of the window procedure in the window's own data.
pub const GWLP_WNDPROC: i32 = -4;
/// The left mouse button.
pub const VK_LBUTTON: i32 = 0x01;
/// The right mouse button.
pub const VK_RBUTTON: i32 = 0x02;
/// The middle mouse button.
pub const VK_MBUTTON: i32 = 0x04;
pub const WM_QUIT: UINT = 0x0012;
pub const WM_DESTROY: UINT = 0x0002;

pub const HWND_MESSAGE: HWND = -3isize as HWND;
pub const HWND_TOP: HWND = std::ptr::null_mut();

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
pub const MOD_NOREPEAT: u32 = 0x4000;

pub const BI_RGB: u32 = 0;
pub const DIB_RGB_COLORS: u32 = 0;
pub const DI_NORMAL: u32 = 0x0003;
pub const SRCCOPY: u32 = 0x00CC0020;

pub const CS_HREDRAW: u32 = 0x0002;
pub const CS_VREDRAW: u32 = 0x0001;

#[repr(C)]
pub struct WNDCLASSW {
    pub style: u32,
    pub lpfnWndProc: Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: HICON,
    pub hCursor: HCURSOR,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: UINT,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: DWORD,
    pub pt: POINT,
}

#[repr(C)]
pub struct BITMAPINFOHEADER {
    pub biSize: u32,
    pub biWidth: i32,
    pub biHeight: i32,
    pub biPlanes: u16,
    pub biBitCount: u16,
    pub biCompression: u32,
    pub biSizeImage: u32,
    pub biXPelsPerMeter: i32,
    pub biYPelsPerMeter: i32,
    pub biClrUsed: u32,
    pub biClrImportant: u32,
}

#[repr(C)]
pub struct BITMAPINFO {
    pub bmiHeader: BITMAPINFOHEADER,
    pub bmiColors: [u32; 3],
}

#[repr(C)]
pub struct ICONINFO {
    pub fIcon: BOOL,
    pub xHotspot: DWORD,
    pub yHotspot: DWORD,
    pub hbmMask: HBITMAP,
    pub hbmColor: HBITMAP,
}

/// `SHFILEINFOW` — what the shell says about a file.
#[repr(C)]
pub struct SHFILEINFO {
    pub hIcon: HICON,
    pub iIcon: i32,
    pub dwAttributes: DWORD,
    pub szDisplayName: [u16; 260],
    pub szTypeName: [u16; 80],
}

/// `SHGFI_ICON` — ask for the icon.
pub const SHGFI_ICON: u32 = 0x0000_0100;
/// `SHGFI_USEFILEATTRIBUTES` — do not touch the disk, take the attributes
/// from the path.
pub const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
/// `SHGFI_SYSICONINDEX` — ask for the place of the icon in the image list of
/// the shell instead of for the icon itself.
pub const SHGFI_SYSICONINDEX: u32 = 0x0000_4000;

#[repr(C)]
pub struct BITMAP {
    pub bmType: i32,
    pub bmWidth: i32,
    pub bmHeight: i32,
    pub bmWidthBytes: i32,
    pub bmPlanes: u16,
    pub bmBitCount: u16,
    pub bmBits: *mut c_void,
}

/// A process entry from `CreateToolhelp32Snapshot`.
#[repr(C)]
pub struct PROCESSENTRY32W {
    pub dwSize: u32,
    pub cntUsage: u32,
    pub th32ProcessID: u32,
    pub th32DefaultHeapID: usize,
    pub th32ModuleID: u32,
    pub cntThreads: u32,
    pub th32ParentProcessID: u32,
    pub pcPriClassBase: i32,
    pub dwFlags: u32,
    pub szExeFile: [u16; 260],
}

/// The privileges Windows needs for the power actions.
pub const SE_SHUTDOWN_PRIVILEGE: &str = "SeShutdownPrivilege";
pub const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
pub const TOKEN_QUERY: u32 = 0x0008;
pub const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
pub const EWX_SHUTDOWN: u32 = 0x00000001;
pub const EWX_REBOOT: u32 = 0x00000002;
pub const EWX_FORCE: u32 = 0x00000004;
pub const EWX_FORCEIFHUNG: u32 = 0x00000010;
pub const EWX_LOGOFF: u32 = 0x00000000;
pub const SHERB_NOCONFIRMATION: u32 = 0x00000001;
pub const SHERB_NOPROGRESSUI: u32 = 0x00000002;
pub const TH32CS_SNAPPROCESS: u32 = 0x00000002;
pub const PROCESS_TERMINATE: u32 = 0x0001;
pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;

/// `ERROR_ALREADY_EXISTS` — another process already owns the named object.
pub const ERROR_ALREADY_EXISTS: u32 = 183;

// ---------------------------------------------------------- named events

/// The right to set a named event.
pub const EVENT_MODIFY_STATE: u32 = 0x0002;
/// `WaitForSingleObject` returned because the object was signalled.
pub const WAIT_OBJECT_0: u32 = 0x0000;
/// `WaitForSingleObject` ran out of time.
pub const WAIT_TIMEOUT: u32 = 0x0102;

// ---------------------------------------------------------------- tray icon

/// A GUID, as used by the shell notification API.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// The memory counters of a process.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PROCESS_MEMORY_COUNTERS {
    pub cb: u32,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
    pub PrivateUsage: usize,
}

/// The data of a shell notification (the tray icon).
#[repr(C)]
pub struct NOTIFYICONDATAW {
    pub cbSize: u32,
    pub hWnd: HWND,
    pub uID: u32,
    pub uFlags: u32,
    pub uCallbackMessage: u32,
    pub hIcon: HICON,
    pub szTip: [u16; 128],
    pub dwState: u32,
    pub dwStateMask: u32,
    pub szInfo: [u16; 256],
    pub uVersion: u32,
    pub szInfoTitle: [u16; 64],
    pub dwInfoFlags: u32,
    pub guidItem: GUID,
    pub hBalloonIcon: HICON,
}

impl Default for NOTIFYICONDATAW {
    fn default() -> Self {
        Self {
            cbSize: 0,
            hWnd: std::ptr::null_mut(),
            uID: 0,
            uFlags: 0,
            uCallbackMessage: 0,
            hIcon: std::ptr::null_mut(),
            szTip: [0; 128],
            dwState: 0,
            dwStateMask: 0,
            szInfo: [0; 256],
            uVersion: 0,
            szInfoTitle: [0; 64],
            dwInfoFlags: 0,
            guidItem: GUID {
                data1: 0,
                data2: 0,
                data3: 0,
                data4: [0; 8],
            },
            hBalloonIcon: std::ptr::null_mut(),
        }
    }
}

/// Add the icon to the tray.
pub const NIM_ADD: u32 = 0x0000_0000;
/// Change the icon.
pub const NIM_MODIFY: u32 = 0x0000_0001;
/// Remove the icon from the tray.
pub const NIM_DELETE: u32 = 0x0000_0002;
/// Set the version of the notification data.
pub const NIM_SETVERSION: u32 = 0x0000_0004;

/// The callback message is delivered.
pub const NIF_MESSAGE: u32 = 0x0000_0001;
/// The icon is shown.
pub const NIF_ICON: u32 = 0x0000_0002;
/// The tip text is shown.
pub const NIF_TIP: u32 = 0x0000_0004;

/// Version 4 of the notification data.
pub const NOTIFYICON_VERSION_4: u32 = 4;

/// A normal menu entry.
pub const MF_STRING: u32 = 0x0000;
/// A separator.
pub const MF_SEPARATOR: u32 = 0x0800;

/// The menu is shown for the right mouse button.
pub const TPM_RIGHTBUTTON: u32 = 0x0002;
/// The command of the chosen entry is returned instead of a message.
pub const TPM_RETURNCMD: u32 = 0x0100;
/// The menu is shown without activating the window.
pub const TPM_NONOTIFY: u32 = 0x0080;

#[link(name = "user32")]
unsafe extern "system" {
    pub fn RegisterHotKey(hwnd: HWND, id: i32, fsModifiers: u32, vk: u32) -> BOOL;
    pub fn UnregisterHotKey(hwnd: HWND, id: i32) -> BOOL;
    pub fn RegisterClassW(lpwcx: *const WNDCLASSW) -> u16;
    pub fn CreateWindowExW(
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;
    pub fn DefWindowProcW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    pub fn DestroyWindow(hwnd: HWND) -> BOOL;
    pub fn GetMessageW(lpmsg: *mut MSG, hwnd: HWND, wMsgFilterMin: UINT, wMsgFilterMax: UINT) -> BOOL;
    pub fn TranslateMessage(lpmsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpmsg: *const MSG) -> LRESULT;
    pub fn PostQuitMessage(nExitCode: i32) -> BOOL;
    pub fn PostMessageW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> BOOL;
    pub fn ShowWindow(hwnd: HWND, nCmdShow: i32) -> BOOL;
    pub fn SetForegroundWindow(hwnd: HWND) -> BOOL;
    pub fn BringWindowToTop(hwnd: HWND) -> BOOL;
    pub fn IsWindowVisible(hwnd: HWND) -> BOOL;
    pub fn GetAsyncKeyState(vkey: i32) -> i16;
    pub fn GetKeyState(vkey: i32) -> i16;
    pub fn SetWindowPos(
        hwnd: HWND,
        hwndInsertAfter: HWND,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> BOOL;
    pub fn GetCursorPos(lppoint: *mut POINT) -> BOOL;
    pub fn GetWindowRect(hwnd: HWND, lprect: *mut RECT) -> BOOL;
    /// The dots per inch of the window.
    ///
    /// Windows 10 1607 and newer. The value is what tells how many physical
    /// pixels one point is worth, and it does not change while the window is
    /// resized — which the size of the window itself does.
    pub fn GetDpiForWindow(hwnd: HWND) -> u32;
    pub fn GetWindowLongW(hwnd: HWND, nindex: i32) -> i32;
    pub fn SetWindowLongPtrW(hwnd: HWND, nindex: i32, dwNewLong: isize) -> isize;
    pub fn GetWindowLongPtrW(hwnd: HWND, nindex: i32) -> isize;
    /// Call a window procedure that was replaced.
    pub fn CallWindowProcW(
        prev: WNDPROC,
        hwnd: HWND,
        msg: UINT,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT;
    /// Start a timer that sends `WM_TIMER` to the window.
    pub fn SetTimer(hwnd: HWND, id: usize, millis: u32, callback: *mut c_void) -> usize;
    /// Stop a timer.
    pub fn KillTimer(hwnd: HWND, id: usize) -> BOOL;
    /// Register (or unregister) the raw input devices.
    pub fn RegisterRawInputDevices(
        devices: *const RAWINPUTDEVICE,
        count: u32,
        size: u32,
    ) -> BOOL;
    pub fn EnumWindows(lpenumfunc: WNDENUMPROC, lparam: LPARAM) -> BOOL;
    pub fn GetWindowTextW(hwnd: HWND, lpstring: *mut u16, nmaxcount: i32) -> i32;
    pub fn GetWindowThreadProcessId(hwnd: HWND, lpdwprocessid: *mut u32) -> u32;
    pub fn GetForegroundWindow() -> HWND;
    pub fn AttachThreadInput(idattach: u32, idattachto: u32, fattach: BOOL) -> BOOL;
    pub fn SetActiveWindow(hwnd: HWND) -> HWND;
    pub fn TerminateProcess(hprocess: HANDLE, uexitcode: u32) -> BOOL;
    pub fn ExitWindowsEx(uexeflags: u32, dwreason: u32) -> BOOL;
    pub fn SetSuspendState(bhibernate: BOOL, bforce: BOOL, bdisablewakeevent: BOOL) -> BOOL;
    pub fn LockWorkStation() -> BOOL;
    pub fn CreateToolhelp32Snapshot(dwflags: u32, th32processid: u32) -> HANDLE;
    pub fn Process32FirstW(hsnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn Process32NextW(hsnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn OpenProcessToken(processthreadhandle: HANDLE, desiredaccess: u32, phtoken: *mut HANDLE) -> BOOL;
    pub fn AdjustTokenPrivileges(
        tokenhandle: HANDLE,
        disableallprivileges: BOOL,
        newstate: *mut TOKEN_PRIVILEGES,
        bufferlength: u32,
        previousstate: *mut TOKEN_PRIVILEGES,
        returnlength: *mut u32,
    ) -> BOOL;
    pub fn LookupPrivilegeValueW(lpsystemname: *const u16, lpname: *const u16, luid: *mut LUID) -> BOOL;
    pub fn GetWindowDC(hwnd: HWND) -> HDC;
    pub fn FindWindowW(lpclassname: *const u16, lpwindowname: *const u16) -> HWND;
    pub fn PrintWindow(hwnd: HWND, hdc: HDC, dwflags: u32) -> BOOL;
    pub fn MonitorFromPoint(pt: POINT, dwFlags: DWORD) -> HMONITOR;
    pub fn GetMonitorInfoW(hmonitor: HMONITOR, lpmi: *mut MONITORINFO) -> BOOL;
    pub fn ExtractIconExW(
        lpszFile: *const u16,
        nIconIndex: i32,
        phiconLarge: *mut HICON,
        phiconSmall: *mut HICON,
        nIcons: u32,
    ) -> u32;
    pub fn DestroyIcon(hIcon: HICON) -> BOOL;
    /// Ask the shell about a file: its icon, its type, its name.
    ///
    /// With `SHGFI_ICON` the shell answers with the icon of the program that
    /// opens the file — the one the user has chosen.
    pub fn SHGetFileInfoW(
        pszpath: *const u16,
        dwfileattributes: DWORD,
        psfi: *mut SHFILEINFO,
        cbfileinfo: u32,
        uflags: u32,
    ) -> usize;
    pub fn GetIconInfo(hIcon: HICON, piconinfo: *mut ICONINFO) -> BOOL;
    pub fn DrawIconEx(
        hdc: HDC,
        xLeft: i32,
        yTop: i32,
        cxWidth: i32,
        cyHeight: i32,
        hIcon: HICON,
        hbmMask: HBITMAP,
        dwFlags: UINT,
    ) -> BOOL;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    pub fn GetDC(hwnd: HWND) -> HDC;
    pub fn ReleaseDC(hwnd: HWND, hdc: HDC) -> i32;
    pub fn InvalidateRect(hwnd: HWND, lpRect: *const RECT, erase: BOOL) -> BOOL;
    pub fn CreateMutexW(
        lpMutexAttributes: *const c_void,
        bInitialOwner: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn Shell_NotifyIconW(dwMessage: u32, lpData: *mut NOTIFYICONDATAW) -> BOOL;
    pub fn RegisterWindowMessageW(lpString: *const u16) -> u32;
    pub fn CreatePopupMenu() -> HMENU;
    pub fn AppendMenuW(
        hmenu: HMENU,
        uFlags: u32,
        uIDNewItem: usize,
        lpNewItem: *const u16,
    ) -> BOOL;
    pub fn DestroyMenu(hmenu: HMENU) -> BOOL;
    pub fn TrackPopupMenu(
        hmenu: HMENU,
        uFlags: u32,
        x: i32,
        y: i32,
        nReserved: i32,
        hwnd: HWND,
        lpRect: *const RECT,
    ) -> usize;
    pub fn BitBlt(
        dest: HDC,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        src: HDC,
        src_x: i32,
        src_y: i32,
        rop: u32,
    ) -> BOOL;
    pub fn CreateCompatibleDC(hdc: HDC) -> HDC;
    pub fn DeleteDC(hdc: HDC) -> BOOL;
    pub fn CreateDIBSection(
        hdc: HDC,
        pbmi: *const BITMAPINFO,
        usage: u32,
        ppvBits: *mut *mut c_void,
        hSection: HANDLE,
        offset: u32,
    ) -> HBITMAP;
    pub fn SelectObject(hdc: HDC, h: HGDIOBJ) -> HGDIOBJ;
    pub fn DeleteObject(ho: HGDIOBJ) -> BOOL;
    pub fn GetDIBits(
        hdc: HDC,
        hbm: HBITMAP,
        start: u32,
        cLines: u32,
        lpvBits: *mut c_void,
        lpbmi: *mut BITMAPINFO,
        usage: u32,
    ) -> i32;
    pub fn GetObjectW(hobject: HANDLE, nbytes: i32, pv: *mut c_void) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    pub fn ShellExecuteW(
        hwnd: HWND,
        lpOperation: *const u16,
        lpFile: *const u16,
        lpParameters: *const u16,
        lpDirectory: *const u16,
        nShowCmd: i32,
    ) -> HINSTANCE;
    pub fn ShellExecuteExW(info: *mut ShellExecuteInfo) -> BOOL;
}

/// `MB_OK | MB_ICONINFORMATION`
pub const MB_OK: u32 = 0x0000_0000;
pub const MB_ICONINFORMATION: u32 = 0x0000_0040;
pub const MB_ICONWARNING: u32 = 0x0000_0030;

#[link(name = "user32")]
unsafe extern "system" {
    /// Show a message box.
    pub fn MessageBoxW(
        hwnd: HWND,
        text: *const u16,
        caption: *const u16,
        kind: u32,
    ) -> i32;
}

/// `STARTUPINFOW` — what a program needs to know about its own start.
#[repr(C)]
pub struct StartupInfoW {
    pub cb: u32,
    pub lpReserved: *const u16,
    pub lpDesktop: *const u16,
    pub lpTitle: *const u16,
    pub dwX: u32,
    pub dwY: u32,
    pub dwXSize: u32,
    pub dwYSize: u32,
    pub dwXCountChars: u32,
    pub dwYCountChars: u32,
    pub dwFillAttribute: u32,
    pub dwFlags: u32,
    pub wShowWindow: u16,
    pub cbReserved2: u16,
    pub lpReserved2: *const u8,
    pub hStdInput: HANDLE,
    pub hStdOutput: HANDLE,
    pub hStdError: HANDLE,
}

/// What a program that was started tells about itself.
#[repr(C)]
pub struct ProcessInformation {
    pub hProcess: HANDLE,
    pub hThread: HANDLE,
    pub dwProcessId: u32,
    pub dwThreadId: u32,
}

/// `CREATE_NO_WINDOW` — the program gets no console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn CreateProcessW(
        lpApplicationName: *const u16,
        lpCommandLine: *mut u16,
        lpProcessAttributes: *const c_void,
        lpThreadAttributes: *const c_void,
        bInheritHandles: BOOL,
        dwCreationFlags: u32,
        lpEnvironment: *const c_void,
        lpCurrentDirectory: *const u16,
        lpStartupInfo: *const StartupInfoW,
        lpProcessInformation: *mut ProcessInformation,
    ) -> BOOL;
}

/// `SHELLEXECUTEINFOW` — the fuller form of [`ShellExecuteW`].
///
/// The two are the same call; this one can say more about how the verb is to
/// be looked up, which is what [`SEE_MASK_INVOKEIDLIST`] is for.
#[repr(C)]
pub struct ShellExecuteInfo {
    /// The size of this structure — `std::mem::size_of` of it.
    pub cb_size: u32,
    /// What the call is to do. See the `SEE_MASK_*` constants.
    pub f_mask: u32,
    /// The window that owns what is started.
    pub hwnd: HWND,
    /// The verb — *open*, *properties* — or nothing for the usual one.
    pub lp_verb: *const u16,
    /// The file the verb is to be applied to.
    pub lp_file: *const u16,
    /// The arguments of the program.
    pub lp_parameters: *const u16,
    /// The folder the program is to be started in.
    pub lp_directory: *const u16,
    /// How the window is to be shown.
    pub n_show: i32,
    /// The handle of the program that was started.
    pub h_inst_app: HINSTANCE,
    /// The list of identifiers, when the file is given as one.
    pub lp_id_list: *mut c_void,
    /// The class of the file.
    pub lp_class: *const u16,
    /// The key of the class.
    pub hkey_class: HANDLE,
    /// The key of the hotkey.
    pub dw_hot_key: u32,
    /// The icon of the program.
    pub h_icon: HANDLE,
    /// The process that was started.
    pub h_process: HANDLE,
}

/// Let the shell look the verb up in the namespace of the shell, and not
/// only in the registry of the file type.
///
/// The verb *properties* is not registered for the file types on Windows 10
/// and newer — the file manager draws it itself — so the plain call answers
/// `SE_ERR_NOASSOC` (31) and nothing happens. With this flag the shell goes
/// through the namespace of the shell, where the verb is found.
pub const SEE_MASK_INVOKEIDLIST: u32 = 0x0000_000C;
/// No separate box about an error.
pub const SEE_MASK_FLAG_NO_UI: u32 = 0x0000_0400;

/// Read access to a file.
pub const GENERIC_READ: u32 = 0x8000_0000;
/// Other processes may read the file while it is open.
pub const FILE_SHARE_READ: u32 = 0x0000_0001;
/// Other processes may write to the file while it is open.
pub const FILE_SHARE_WRITE: u32 = 0x0000_0002;
/// Other processes may delete the file while it is open.
pub const FILE_SHARE_DELETE: u32 = 0x0000_0004;
/// Open an existing file, fail if it is not there.
pub const OPEN_EXISTING: u32 = 3;
/// The usual attributes of an ordinary file.
pub const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
/// A folder may be opened like a file — this is what says so.
pub const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
/// A read-only mapping.
pub const PAGE_READONLY: u32 = 0x02;
/// Map the file for reading.
pub const FILE_MAP_READ: u32 = 0x0004;

#[link(name = "kernel32")]
unsafe extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HINSTANCE;
    pub fn GetLocalTime(lpSystemTime: *mut SYSTEMTIME);
    pub fn CloseHandle(hobject: HANDLE) -> BOOL;
    pub fn CreateEventW(
        lpEventAttributes: *const c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn OpenEventW(dwDesiredAccess: u32, bInheritHandle: BOOL, lpName: *const u16) -> HANDLE;
    pub fn SetEvent(hEvent: HANDLE) -> BOOL;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: u32) -> u32;
    pub fn GetCurrentProcess() -> HANDLE;
    pub fn GetLastError() -> u32;
    pub fn GetCurrentThreadId() -> u32;
    pub fn OpenProcess(dwdesiredaccess: u32, binherithandle: BOOL, dwprocessid: u32) -> HANDLE;
    pub fn QueryFullProcessImageNameW(
        hprocess: HANDLE,
        dwflags: u32,
        lpexefilename: *mut u16,
        lpdwsize: *mut u32,
    ) -> BOOL;
    pub fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    /// Read from a pipe or a file.
    pub fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut c_void,
        nNumberOfBytesToRead: u32,
        lpNumberOfBytesRead: *mut u32,
        lpOverlapped: *mut c_void,
    ) -> BOOL;
    /// Write to a pipe or a file.
    pub fn WriteFile(
        hFile: HANDLE,
        lpBuffer: *const c_void,
        nNumberOfBytesToWrite: u32,
        lpNumberOfBytesWritten: *mut u32,
        lpOverlapped: *mut c_void,
    ) -> BOOL;
    pub fn CreateFileMappingW(
        hFile: HANDLE,
        lpFileMappingAttributes: *const c_void,
        flProtect: u32,
        dwMaximumSizeHigh: u32,
        dwMaximumSizeLow: u32,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn MapViewOfFile(
        hFileMappingObject: HANDLE,
        dwDesiredAccess: u32,
        dwFileOffsetHigh: u32,
        dwFileOffsetLow: u32,
        dwNumberOfBytesToMap: usize,
    ) -> *mut c_void;
    pub fn UnmapViewOfFile(lpBaseAddress: *mut c_void) -> BOOL;
    pub fn GetFileSizeEx(hFile: HANDLE, lpFileSize: *mut i64) -> BOOL;
    /// The size the file takes on the disk, which is not its length when
    /// the file system packs it into clusters.
    pub fn GetCompressedFileSizeW(lpFileName: *const u16, lpFileSizeHigh: *mut u32) -> u32;
    /// The identity of an open file: its index number, the volume it is on
    /// and how many names point at it.
    pub fn GetFileInformationByHandle(
        hFile: HANDLE,
        lpFileInformation: *mut BY_HANDLE_FILE_INFORMATION,
    ) -> BOOL;
    /// The first alternate data stream of a file.
    ///
    /// A file can carry streams besides its content — `Zone.Identifier` is
    /// the one Windows writes when a file comes from the internet.
    pub fn FindFirstStreamW(
        lpFileName: *const u16,
        dwInfoLevel: u32,
        lpStreamData: *mut WIN32_FIND_STREAM_DATA,
        dwFlags: u32,
    ) -> HANDLE;
    pub fn FindNextStreamW(
        hFindStream: HANDLE,
        lpStreamData: *mut WIN32_FIND_STREAM_DATA,
    ) -> BOOL;
    pub fn FindClose(hFindFile: HANDLE) -> BOOL;
    /// How much room is left on a volume.
    pub fn GetDiskFreeSpaceExW(
        lpDirectoryName: *const u16,
        lpFreeBytesAvailableToCaller: *mut u64,
        lpTotalNumberOfBytes: *mut u64,
        lpTotalNumberOfFreeBytes: *mut u64,
    ) -> BOOL;
    /// The label, the kind and the serial number of a volume.
    pub fn GetVolumeInformationW(
        lpRootPathName: *const u16,
        lpVolumeNameBuffer: *mut u16,
        nVolumeNameSize: u32,
        lpVolumeSerialNumber: *mut u32,
        lpMaximumComponentLength: *mut u32,
        lpFileSystemFlags: *mut u32,
        lpFileSystemNameBuffer: *mut u16,
        nFileSystemNameSize: u32,
    ) -> BOOL;
}

/// `BY_HANDLE_FILE_INFORMATION` — what the file system knows about a file.
#[repr(C)]
pub struct BY_HANDLE_FILE_INFORMATION {
    pub dwFileAttributes: u32,
    pub ftCreationTime: FILETIME,
    pub ftLastAccessTime: FILETIME,
    pub ftLastWriteTime: FILETIME,
    pub dwVolumeSerialNumber: u32,
    pub nFileSizeHigh: u32,
    pub nFileSizeLow: u32,
    pub nNumberOfLinks: u32,
    pub nFileIndexHigh: u32,
    pub nFileIndexLow: u32,
}

/// `FILETIME` — a timestamp in 100 nanoseconds since 1601.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FILETIME {
    pub dwLowDateTime: u32,
    pub dwHighDateTime: u32,
}

/// `WIN32_FIND_STREAM_DATA` — the name of one alternate data stream.
#[repr(C)]
pub struct WIN32_FIND_STREAM_DATA {
    pub StreamSize: i64,
    pub cStreamName: [u16; 296],
}

#[link(name = "shell32")]
unsafe extern "system" {
    pub fn SHEmptyRecycleBinW(hwnd: HWND, lpszrootpath: *const u16, dwflags: u32) -> i32;
    /// Ask where the shell put the tray icon.
    pub fn Shell_NotifyIconGetRect(hwnd: HWND, uid: u32, lprect: *mut RECT) -> BOOL;
}

/// The name of the kind of a file, as the shell calls it.
pub const SHGFI_TYPENAME: u32 = 0x0000_0400;

#[link(name = "shlwapi")]
unsafe extern "system" {
    /// Ask the shell which program handles a kind of file.
    ///
    /// `pszAssoc` is the extension (with the dot) and `pszExtra` says what
    /// is wanted: `"executable"` for the program itself, `"command"` for
    /// the whole command line.
    pub fn AssocQueryStringW(
        flags: u32,
        pszAssoc: *const u16,
        pszExtra: *const u16,
        pszOut: *mut u16,
        pcchOut: *mut u32,
    ) -> i32;
}

/// The shell is to ignore the kinds it does not know.
pub const ASSOCF_INIT_IGNOREUNKNOWN: u32 = 0x0000_0010;

/// The window attribute that holds the corner rounding preference.
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;

/// The window attribute that holds the colour of the window border.
pub const DWMWA_BORDER_COLOR: u32 = 34;

/// The window attribute that holds the kind of backdrop behind the window.
pub const DWMWA_SYSTEMBACKDROP_TYPE: u32 = 38;

/// The older window attribute that turns the mica effect on or off.
pub const DWMWA_MICA_EFFECT: u32 = 1029;

/// No backdrop at all: the window is drawn on whatever is behind it.
///
/// Windows 11 puts an acrylic backdrop behind every window that looks like
/// an ordinary application window, and the launcher is not one — the card it
/// draws is the whole window. Left alone, the system therefore paints a
/// blurred sheet around the card, and the launcher ends up looking like two
/// layers instead of one.
pub const DWMSBT_NONE: i32 = 1;

/// Do not draw a border around the window.
///
/// The value is `0xFFFFFFFE`, which does not fit into a signed integer.
pub const DWMWA_COLOR_NONE: i32 = -2;

/// Round the corners of the window, the way an ordinary window is rounded.
///
/// Windows 11 rounds every top level window anyway; this only says that the
/// launcher wants the standard curve rather than a square one.
pub const DWMWCP_ROUND: i32 = 2;

/// Do not round the corners of the window.
///
/// Windows 11 rounds every top level window by default, with a radius of its
/// own. A window that draws its own rounded card would then be clipped by a
/// *second*, different radius — and the sliver between the two arcs shows
/// the desktop.
pub const DWMWCP_DONOTROUND: i32 = 1;

#[link(name = "dwmapi")]
unsafe extern "system" {
    pub fn DwmSetWindowAttribute(
        hwnd: HWND,
        dwAttribute: u32,
        pvAttribute: *const c_void,
        cbAttribute: u32,
    ) -> i32;
}

#[repr(C)]
pub struct SYSTEMTIME {
    pub wYear: u16,
    pub wMonth: u16,
    pub wDayOfWeek: u16,
    pub wDay: u16,
    pub wHour: u16,
    pub wMinute: u16,
    pub wSecond: u16,
    pub wMilliseconds: u16,
}

#[link(name = "psapi")]
unsafe extern "system" {
    pub fn GetProcessMemoryInfo(
        process: HANDLE,
        counters: *mut PROCESS_MEMORY_COUNTERS,
        cb: u32,
    ) -> BOOL;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    pub fn RegGetValueW(
        hkey: HKEY,
        lpsubkey: *const u16,
        lpsubkey2: *const u16,
        dwflags: u32,
        dwtype: *mut u32,
        pvdata: *mut c_void,
        pcbdata: *mut u32,
    ) -> i32;
    pub fn RegOpenKeyExW(
        hkey: HKEY,
        lpsubkey: *const u16,
        uloptions: u32,
        samdesired: u32,
        phkresult: *mut HKEY,
    ) -> i32;
    pub fn RegEnumKeyExW(
        hkey: HKEY,
        dwindex: u32,
        lpname: *mut u16,
        lpcchname: *mut u32,
        lpreserved: *mut u32,
        lpclass: *mut u16,
        lpcchclass: *mut u32,
        lpft: *mut u32,
    ) -> i32;
    pub fn RegSetValueExW(
        hkey: HKEY,
        lpvaluename: *const u16,
        reserved: u32,
        dwtype: u32,
        lpdata: *const u8,
        cbdata: u32,
    ) -> i32;
    pub fn RegCloseKey(hkey: HKEY) -> i32;
}

pub const HKEY_CURRENT_USER: HKEY = 0x8000_0001usize as HKEY;
/// The classes of the files — the registry where the kinds of files live.
pub const HKEY_CLASSES_ROOT: HKEY = 0x8000_0000usize as HKEY;
pub const RRF_RT_REG_DWORD: u32 = 0x0000_0018;
/// Read a string value.
pub const RRF_RT_REG_SZ: u32 = 0x0000_0002;
/// The right to read a key.
pub const KEY_READ: u32 = 0x0002_0019;
/// The right to write a value.
pub const KEY_SET_VALUE: u32 = 0x0000_0002;
/// A value that holds a number.
pub const REG_DWORD: u32 = 4;

/// Read `AppsUseLightTheme` from the personalisation key.
///
/// Returns `true` when Windows is in dark mode.
pub fn registry_light_theme_prefers_dark() -> bool {
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();

    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let mut kind: u32 = 0;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            &mut kind,
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };

    if status != 0 {
        return false;
    }
    // AppsUseLightTheme == 0 means "dark".
    data == 0
}

pub type HMONITOR = *mut c_void;

/// A locally-unique identifier, used for privileges.
#[repr(C)]
pub struct LUID {
    pub low_part: u32,
    pub high_part: i32,
}

/// The privilege state passed to `AdjustTokenPrivileges`.
#[repr(C)]
pub struct TOKEN_PRIVILEGES {
    pub privilege_count: u32,
    pub privileges: [LUID_AND_ATTRIBUTES; 1],
}

/// One privilege and its attributes.
#[repr(C)]
pub struct LUID_AND_ATTRIBUTES {
    pub luid: LUID,
    pub attributes: u32,
}

/// The callback used by `EnumWindows`.
pub type WNDENUMPROC = Option<unsafe extern "system" fn(HWND, LPARAM) -> BOOL>;

#[repr(C)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct MONITORINFO {
    pub cbSize: u32,
    pub rcMonitor: RECT,
    pub rcWork: RECT,
    pub dwFlags: u32,
}

/// Virtual-key codes for the keys a hotkey may use.
pub mod vk {
    pub const BACK: u32 = 0x08;
    pub const TAB: u32 = 0x09;
    pub const RETURN: u32 = 0x0D;
    pub const ESCAPE: u32 = 0x1B;
    pub const SPACE: u32 = 0x20;
    pub const PRIOR: u32 = 0x21;
    pub const NEXT: u32 = 0x22;
    pub const END: u32 = 0x23;
    pub const HOME: u32 = 0x24;
    pub const LEFT: u32 = 0x25;
    pub const UP: u32 = 0x26;
    pub const RIGHT: u32 = 0x27;
    pub const DOWN: u32 = 0x28;
    pub const INSERT: u32 = 0x2D;
    pub const DELETE: u32 = 0x2E;
    pub const KEY0: u32 = 0x30;
    pub const KEY1: u32 = 0x31;
    pub const KEY2: u32 = 0x32;
    pub const KEY3: u32 = 0x33;
    pub const KEY4: u32 = 0x34;
    pub const KEY5: u32 = 0x35;
    pub const KEY6: u32 = 0x36;
    pub const KEY7: u32 = 0x37;
    pub const KEY8: u32 = 0x38;
    pub const KEY9: u32 = 0x39;
    pub const SHIFT: u32 = 0x10;
    pub const CONTROL: u32 = 0x11;
    pub const MENU: u32 = 0x12;
    pub const A: u32 = 0x41;
    pub const B: u32 = 0x42;
    pub const C: u32 = 0x43;
    pub const D: u32 = 0x44;
    pub const E: u32 = 0x45;
    pub const F: u32 = 0x46;
    pub const G: u32 = 0x47;
    pub const H: u32 = 0x48;
    pub const I: u32 = 0x49;
    pub const J: u32 = 0x4A;
    pub const K: u32 = 0x4B;
    pub const L: u32 = 0x4C;
    pub const M: u32 = 0x4D;
    pub const N: u32 = 0x4E;
    pub const O: u32 = 0x4F;
    pub const P: u32 = 0x50;
    pub const Q: u32 = 0x51;
    pub const R: u32 = 0x52;
    pub const S: u32 = 0x53;
    pub const T: u32 = 0x54;
    pub const U: u32 = 0x55;
    pub const V: u32 = 0x56;
    pub const W: u32 = 0x57;
    pub const X: u32 = 0x58;
    pub const Y: u32 = 0x59;
    pub const Z: u32 = 0x5A;
    pub const F1: u32 = 0x70;
    pub const F12: u32 = 0x7B;
}

/// Parse a hotkey description such as `"Alt + Space"` into modifiers and a
/// virtual-key code.
///
/// Returns `None` when the description cannot be understood.
pub fn parse_hotkey(description: &str) -> Option<(u32, u32)> {
    let mut modifiers = 0u32;
    let mut key: Option<u32> = None;

    for part in description.split(['+', '-']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "alt" => modifiers |= MOD_ALT,
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "shift" => modifiers |= MOD_SHIFT,
            "win" | "super" | "meta" => modifiers |= MOD_WIN,
            "space" => key = Some(vk::SPACE),
            "enter" | "return" => key = Some(vk::RETURN),
            "esc" | "escape" => key = Some(vk::ESCAPE),
            "tab" => key = Some(vk::TAB),
            "up" => key = Some(vk::UP),
            "down" => key = Some(vk::DOWN),
            "left" => key = Some(vk::LEFT),
            "right" => key = Some(vk::RIGHT),
            "pageup" | "page up" => key = Some(vk::PRIOR),
            "pagedown" | "page down" => key = Some(vk::NEXT),
            "home" => key = Some(vk::HOME),
            "end" => key = Some(vk::END),
            "delete" | "del" => key = Some(vk::DELETE),
            "insert" | "ins" => key = Some(vk::INSERT),
            "backspace" => key = Some(vk::BACK),
            other => {
                let mut chars = other.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    match c {
                        'a'..='z' => key = Some(vk::A + (c as u32 - 'a' as u32)),
                        '0'..='9' => key = Some(vk::KEY0 + (c as u32 - '0' as u32)),
                        _ => {}
                    }
                }
            }
        }
    }

    key.map(|k| (modifiers, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_alt_space() {
        let (mods, key) = parse_hotkey("Alt + Space").unwrap();
        assert_eq!(mods, MOD_ALT);
        assert_eq!(key, vk::SPACE);
    }

    #[test]
    fn parses_ctrl_shift_letter() {
        let (mods, key) = parse_hotkey("Ctrl + Shift + K").unwrap();
        assert_eq!(mods, MOD_CONTROL | MOD_SHIFT);
        assert_eq!(key, vk::K);
    }

    #[test]
    fn parses_single_letter() {
        let (mods, key) = parse_hotkey("j").unwrap();
        assert_eq!(mods, 0);
        assert_eq!(key, vk::J);
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_hotkey("   ").is_none());
    }
}
