use super::*;

#[repr(C)]

/// The fields of one record of the master file table.
///
/// The name of the record is not part of it: it is handed over separately,
/// because it has to be turned into text.
pub(in crate::usn) struct SecurityAttributes {
    pub(in crate::usn) length: u32,
    pub(in crate::usn) descriptor: *mut c_void,
    pub(in crate::usn) inherit_handle: Bool,
}

#[repr(C)]
pub(in crate::usn) struct UnicodeString {
    pub(in crate::usn) length: u16,
    pub(in crate::usn) maximum_length: u16,
    pub(in crate::usn) buffer: *mut u16,
}

#[repr(C)]
pub(in crate::usn) struct ObjectAttributes {
    pub(in crate::usn) length: u32,
    pub(in crate::usn) root_directory: Handle,
    pub(in crate::usn) object_name: *mut UnicodeString,
    pub(in crate::usn) attributes: u32,
    pub(in crate::usn) security_descriptor: *mut c_void,
    pub(in crate::usn) security_quality_of_service: *mut c_void,
}

#[repr(C)]
pub(in crate::usn) struct IoStatusBlock {
    pub(in crate::usn) status: NtStatus,
    pub(in crate::usn) information: usize,
}

/// The locally unique identifier of a privilege.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(in crate::usn) struct Luid {
    pub(in crate::usn) low_part: u32,
    pub(in crate::usn) high_part: u32,
}

/// A privilege together with the attributes it should get.
#[repr(C)]
pub(in crate::usn) struct LuidAndAttributes {
    pub(in crate::usn) luid: Luid,
    pub(in crate::usn) attributes: u32,
}

/// The list of privileges that `AdjustTokenPrivileges` changes.
#[repr(C)]
pub(in crate::usn) struct TokenPrivileges {
    pub(in crate::usn) privilege_count: u32,
    pub(in crate::usn) control: u32,
    pub(in crate::usn) privileges: [LuidAndAttributes; 1],
}

#[repr(C)]
pub(in crate::usn) struct FileIdDescriptor {
    pub(in crate::usn) volume_handle: Handle,
    pub(in crate::usn) file_id: [u8; 16],
}

/// `MFT_ENUM_DATA_V0`, the input of `FSCTL_ENUM_USN_DATA`.
#[repr(C)]
pub(in crate::usn) struct MftEnumDataV0 {
    pub(in crate::usn) start_file_reference_number: u64,
    pub(in crate::usn) low_usn: i64,
    pub(in crate::usn) high_usn: i64,
}

/// `USN_JOURNAL_DATA_V0`, the output of `FSCTL_READ_USN_JOURNAL`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]

pub struct UsnJournalDataV0 {
    pub usn_journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    pub maximum_size: u64,
    pub allocation_delta: u64,
}

#[link(name = "kernel32")]

unsafe extern "system" {
    pub(in crate::usn) fn CreateFileW(
        file_name: *const u16,
        desired_access: Dword,
        share_mode: Dword,
        security_attributes: *mut SecurityAttributes,
        creation_disposition: Dword,
        flags_and_attributes: Dword,
        template_file: Handle,
    ) -> Handle;
    pub(in crate::usn) fn CloseHandle(object: Handle) -> Bool;
    pub(in crate::usn) fn GetCurrentProcess() -> Handle;
    pub(in crate::usn) fn OpenProcessToken(process: Handle, desired_access: Dword, token: *mut Handle) -> Bool;
    pub(in crate::usn) fn DeviceIoControl(
        device: Handle,
        control_code: Dword,
        in_buffer: *const c_void,
        in_buffer_size: Dword,
        out_buffer: *mut c_void,
        out_buffer_size: Dword,
        bytes_returned: *mut Dword,
        overlapped: *mut c_void,
    ) -> Bool;
    pub(in crate::usn) fn GetLastError() -> Dword;
    pub(in crate::usn) fn SetLastError(code: Dword);
    pub(in crate::usn) fn GetDriveTypeW(root_path: *const u16) -> u32;
    pub(in crate::usn) fn GetLogicalDrives() -> Dword;
    pub(in crate::usn) fn GetVolumeInformationW(
        root_path: *const u16,
        volume_name_buffer: *mut u16,
        volume_name_size: Dword,
        volume_serial_number: *mut Dword,
        maximum_component_length: *mut Dword,
        file_system_flags: *mut Dword,
        file_system_name_buffer: *mut u16,
        file_system_name_size: Dword,
    ) -> Bool;
}

#[link(name = "ntdll")]

unsafe extern "system" {
    pub(in crate::usn) fn NtOpenFile(
        file_handle: *mut Handle,
        desired_access: Dword,
        object_attributes: *mut ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        allocation_size: *mut i64,
        share_access: Dword,
        create_disposition: Dword,
        create_options: Dword,
        ea_buffer: *mut c_void,
        ea_length: Dword,
    ) -> NtStatus;
    /// Turn a privilege on by its number.
    ///
    /// This is the helper the older tools use; it takes the privilege number
    /// rather than its name and needs no buffer of its own.
    pub(in crate::usn) fn RtlAdjustPrivilege(
        privilege: u32,
        enable: Bool,
        new_state: *mut TokenPrivileges,
        old_state: *mut TokenPrivileges,
    ) -> NtStatus;
}

#[link(name = "advapi32")]

unsafe extern "system" {
    pub(in crate::usn) fn GetTokenInformation(
        token: Handle,
        token_information_class: i32,
        token_information: *mut c_void,
        token_information_length: Dword,
        return_length: *mut Dword,
    ) -> Bool;
    pub(in crate::usn) fn LookupPrivilegeValueW(
        system_name: *const u16,
        name: *const u16,
        luid: *mut Luid,
    ) -> Bool;
    pub(in crate::usn) fn LookupPrivilegeNameW(
        system_name: *const u16,
        luid: *mut Luid,
        name: *mut u16,
        size: *mut Dword,
    ) -> Bool;
    pub(in crate::usn) fn AdjustTokenPrivileges(
        token: Handle,
        disable_all_privileges: Bool,
        new_state: *mut TokenPrivileges,
        buffer_length: Dword,
        previous_state: *mut c_void,
        return_length: *mut Dword,
    ) -> Bool;
}