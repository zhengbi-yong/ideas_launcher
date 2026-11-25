use super::*;


/// True when the process runs with an elevated token.
///
/// Reading the master file table needs it. Everything asks for the same in
/// its own words:
/// `"Everything" requires administrative privileges to index NTFS volumes.`

pub fn is_elevated() -> bool {
    const TOKEN_QUERY: Dword = 0x0008;
    const TOKEN_ELEVATION_CLASS: i32 = 20;

    #[repr(C)]
    struct TokenElevation {
        token_is_elevated: u32,
    }

    let mut token: Handle = std::ptr::null_mut();
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if ok == 0 {
        return false;
    }
    let mut elevation = TokenElevation { token_is_elevated: 0 };
    let mut returned = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_ELEVATION_CLASS,
            (&mut elevation as *mut TokenElevation).cast(),
            std::mem::size_of::<TokenElevation>() as Dword,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.token_is_elevated != 0
}

/// The raw privilege buffer of the process token.
///
/// Used to find out how the privileges are laid out and which identifier the
/// volume privilege carries.
pub fn token_privilege_bytes() -> Vec<u8> {
    const TOKEN_QUERY: Dword = 0x0008;
    const TOKEN_PRIVILEGES_CLASS: i32 = 3;

    let mut token: Handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Vec::new();
    }
    let size = 4096u32;
    let mut buffer = vec![0u8; size as usize];
    let mut returned = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TOKEN_PRIVILEGES_CLASS,
            buffer.as_mut_ptr().cast(),
            size,
            &mut returned,
        )
    };
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Vec::new();
    }
    buffer.truncate(returned as usize);
    buffer
}

/// The privileges the process token holds, with their attributes.
///
/// Used to find out why a privilege could not be turned on.
pub fn token_privileges() -> Vec<(String, u32)> {
    const TOKEN_QUERY: Dword = 0x0008;
    const TOKEN_PRIVILEGES_CLASS: i32 = 3;

    let mut token: Handle = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut size = 1024u32;
    for _ in 0..8 {
        let mut buffer = vec![0u8; size as usize];
        let mut returned = 0u32;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TOKEN_PRIVILEGES_CLASS,
                buffer.as_mut_ptr().cast(),
                size,
                &mut returned,
            )
        };
        if ok == 0 {
            size *= 2;
            continue;
        }
        let count = u32::from_le_bytes(buffer[0..4].try_into().unwrap()) as usize;
        // Each entry is a LUID (8 bytes) followed by the attributes.
        for index in 0..count {
            let base = 8 + index * 12;
            if base + 12 > buffer.len() {
                break;
            }
            let low = u32::from_le_bytes(buffer[base..base + 4].try_into().unwrap());
            let high = u32::from_le_bytes(buffer[base + 4..base + 8].try_into().unwrap());
            let attributes = u32::from_le_bytes(buffer[base + 8..base + 12].try_into().unwrap());
            let mut name_buffer = [0u16; 256];
            let mut name_length = name_buffer.len() as Dword;
            let ok = unsafe {
                LookupPrivilegeNameW(
                    std::ptr::null(),
                    &mut Luid {
                        low_part: low,
                        high_part: high,
                    },
                    name_buffer.as_mut_ptr(),
                    &mut name_length,
                )
            };
            let name = if ok == 0 {
                format!("luid:{low:08x}:{high:08x}")
            } else {
                let end = name_buffer
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(name_buffer.len());
                String::from_utf16_lossy(&name_buffer[..end])
            };
            out.push((name, attributes));
        }
        break;
    }
    unsafe { CloseHandle(token) };
    out
}

/// What happened while the privilege was being turned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivilegeOutcome {
    /// True when the privilege is on.
    pub enabled: bool,
    /// The step that failed, when it did.
    pub stage: &'static str,
    /// The error code of that step.
    pub error: u32,
    /// The identifier of the privilege, for comparing with the token.
    pub luid: (u32, u32),
}

/// The number of the "manage the volume" privilege.
///
/// The numbers are the ones the system uses; `SeManageVolumePrivilege` is
/// number 15.
pub const PRIVILEGE_MANAGE_VOLUME: u32 = 15;

/// Turn on a privilege by name and report what happened.
///
/// `(enabled, stage, error)`.
///
/// Several ways of doing this are tried, because the exact shape of the
/// privilege list is one of those things the headers do not spell out and
/// that has to be found by asking the system. Whichever way works wins, and
/// the others are reported as failures.
pub fn enable_privilege(name: &str) -> (bool, &'static str, u32) {
    const TOKEN_ADJUST_PRIVILEGES: Dword = 0x0020;
    const TOKEN_QUERY: Dword = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;

    let mut token: Handle = std::ptr::null_mut();
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return (false, "OpenProcessToken", unsafe { GetLastError() });
    }
    let wide_name = wide(name);
    let mut luid = Luid {
        low_part: 0,
        high_part: 0,
    };
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), wide_name.as_ptr(), &mut luid) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        return (false, "LookupPrivilegeValueW", error);
    }

    let mut last_error = 0u32;
    for layout in 0..3 {
        // 0: count, control, entry — the shape from the header.
        // 1: count, entry — the shape the system hands back.
        // 2: count, control, padding, entry.
        let mut buffer = [0u8; 24];
        let entry = match layout {
            0 => 8usize,
            1 => 4usize,
            _ => 12usize,
        };
        buffer[0..4].copy_from_slice(&1u32.to_le_bytes());
        buffer[entry..entry + 4].copy_from_slice(&luid.low_part.to_le_bytes());
        buffer[entry + 4..entry + 8].copy_from_slice(&luid.high_part.to_le_bytes());
        buffer[entry + 8..entry + 12].copy_from_slice(&SE_PRIVILEGE_ENABLED.to_le_bytes());

        let mut returned = 0u32;
        unsafe { SetLastError(0) };
        let ok = unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                buffer.as_mut_ptr().cast(),
                (entry + 12) as Dword,
                std::ptr::null_mut(),
                &mut returned,
            )
        };
        let error = unsafe { GetLastError() };
        if ok != 0 && error == 0 {
            unsafe { CloseHandle(token) };
            return (true, layout_name(layout), 0);
        }
        last_error = error;
    }
    unsafe { CloseHandle(token) };

    // The other way round: the helper from ntdll, which knows the layout by
    // itself and only needs the number of the privilege.
    let number = privilege_number(name);
    if number != 0 {
        let status = unsafe {
            RtlAdjustPrivilege(number, 1, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if status == 0 {
            return (true, "RtlAdjustPrivilege", 0);
        }
        last_error = status as u32;
    }
    (false, "AdjustTokenPrivileges", last_error)
}

/// The name of a privilege list layout.
pub(in crate::usn) fn layout_name(layout: usize) -> &'static str {
    match layout {
        0 => "AdjustTokenPrivileges/header",
        1 => "AdjustTokenPrivileges/count-only",
        _ => "AdjustTokenPrivileges/padded",
    }
}

/// The number of a privilege, for [`RtlAdjustPrivilege`].
pub(in crate::usn) fn privilege_number(name: &str) -> u32 {
    match name {
        "SeCreateTokenPrivilege" => 2,
        "SeIncreaseQuotaPrivilege" => 3,
        "SeIncreaseBasePriorityPrivilege" => 4,
        "SeAssignPrimaryTokenPrivilege" => 5,
        "SeTcbPrivilege" => 6,
        "SeSecurityPrivilege" => 7,
        "SeTakeOwnershipPrivilege" => 8,
        "SeLoadDriverPrivilege" => 9,
        "SeSystemProfilePrivilege" => 10,
        "SeSystemtimePrivilege" => 11,
        "SeChangeNotifyPrivilege" => 12,
        "SeRemoteShutdownPrivilege" => 13,
        "SeUndockPrivilege" => 14,
        "SeManageVolumePrivilege" => PRIVILEGE_MANAGE_VOLUME,
        "SeCreateGlobalPrivilege" => 16,
        "SeDebugPrivilege" => 20,
        "SeImpersonatePrivilege" => 21,
        "SeCreateSymbolicLinkPrivilege" => 22,
        _ => 0,
    }
}

/// Turn on the "Manage the volume" privilege.
///
/// This is the piece that is easy to miss. An administrator account *has*
/// the privilege, but it is disabled by default and has to be switched on
/// explicitly with `AdjustTokenPrivileges` — which is exactly what `fsutil`
/// does before it reads a change journal. Without it, `FSCTL_ENUM_USN_DATA`
/// answers `ERROR_ACCESS_DENIED` even in an elevated process.
pub fn enable_manage_volume_privilege() -> bool {
    enable_manage_volume_privilege_detailed().enabled
}

/// Turn on the privilege and report where it went wrong.
pub fn enable_manage_volume_privilege_detailed() -> PrivilegeOutcome {
    const TOKEN_ADJUST_PRIVILEGES: Dword = 0x0020;
    const TOKEN_QUERY: Dword = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;

    let mut token: Handle = std::ptr::null_mut();
    let ok = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if ok == 0 {
        return PrivilegeOutcome {
            enabled: false,
            stage: "OpenProcessToken",
            error: unsafe { GetLastError() },
            luid: (0, 0),
        };
    }

    let name = wide("SeManageVolumePrivilege");
    let mut luid = Luid {
        low_part: 0,
        high_part: 0,
    };
    let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) };
    if ok == 0 {
        let error = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        return PrivilegeOutcome {
            enabled: false,
            stage: "LookupPrivilegeValueW",
            error,
            luid: (luid.low_part, luid.high_part),
        };
    }

    let mut last_error = 0u32;
    for layout in 0..3 {
        // 0: count, control, entry — the shape from the header.
        // 1: count, entry — the shape the system hands back.
        // 2: count, control, padding, entry.
        let mut buffer = [0u8; 24];
        let entry = match layout {
            0 => 8usize,
            1 => 4usize,
            _ => 12usize,
        };
        buffer[0..4].copy_from_slice(&1u32.to_le_bytes());
        buffer[entry..entry + 4].copy_from_slice(&luid.low_part.to_le_bytes());
        buffer[entry + 4..entry + 8].copy_from_slice(&luid.high_part.to_le_bytes());
        buffer[entry + 8..entry + 12].copy_from_slice(&SE_PRIVILEGE_ENABLED.to_le_bytes());

        let mut returned = 0u32;
        unsafe { SetLastError(0) };
        let ok = unsafe {
            AdjustTokenPrivileges(
                token,
                0,
                buffer.as_mut_ptr().cast(),
                (entry + 12) as Dword,
                std::ptr::null_mut(),
                &mut returned,
            )
        };
        let error = unsafe { GetLastError() };
        if ok != 0 && error == 0 {
            unsafe { CloseHandle(token) };
            return PrivilegeOutcome {
                enabled: true,
                stage: layout_name(layout),
                error: 0,
                luid: (luid.low_part, luid.high_part),
            };
        }
        last_error = error;
    }
    unsafe { CloseHandle(token) };

    // The other way round: the helper from ntdll, which knows the layout by
    // itself and only needs the number of the privilege.
    let status = unsafe {
        RtlAdjustPrivilege(
            PRIVILEGE_MANAGE_VOLUME,
            1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if status == 0 {
        return PrivilegeOutcome {
            enabled: true,
            stage: "RtlAdjustPrivilege",
            error: 0,
            luid: (luid.low_part, luid.high_part),
        };
    }
    PrivilegeOutcome {
        enabled: false,
        stage: "AdjustTokenPrivileges",
        error: last_error,
        luid: (luid.low_part, luid.high_part),
    }
}