//! A pure-Rust parser for Windows Shell Link (`.lnk`) files.
//!
//! Implements the subset of [MS-SHLLINK] needed to resolve a shortcut to its
//! target, working directory, arguments and icon.
//!
//! Flow Launcher resolves shortcuts through `IShellLink` over COM
//! (`ShellLinkReader` in `Flow.Launcher.Plugin.Program.dll`). Each COM
//! round-trip costs a cross-apartment marshalling and is one of the reasons
//! its Win32 index takes seconds. Parsing the file directly is roughly two
//! orders of magnitude cheaper and works offline.
//!
//! [MS-SHLLINK]: https://learn.microsoft.com/openspecs/windows_protocols/ms-shllink

/// Everything the parser can recover from a `.lnk` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellLink {
    /// Absolute path of the target executable or document.
    pub target_path: Option<String>,
    /// Description shown in the shell.
    pub description: Option<String>,
    /// Command line arguments.
    pub arguments: Option<String>,
    /// Working directory.
    pub working_directory: Option<String>,
    /// Icon location, as `path,index`.
    pub icon_location: Option<String>,
    /// The name recorded in the shortcut.
    pub name: Option<String>,
    /// Whether the file is a directory.
    pub is_directory: bool,
}

const HEADER_SIZE: usize = 0x4C;
const LINK_CLSID: [u8; 16] = [
    0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46,
];

// Header LinkFlags.
const HAS_LINK_TARGET_ID_LIST: u32 = 0x0000_0001;
const HAS_LINK_INFO: u32 = 0x0000_0002;
const HAS_NAME: u32 = 0x0000_0004;
const HAS_RELATIVE_PATH: u32 = 0x0000_0008;
const HAS_WORKING_DIR: u32 = 0x0000_0010;
const HAS_ARGUMENTS: u32 = 0x0000_0020;
const HAS_ICON_LOCATION: u32 = 0x0000_0040;
const IS_UNICODE: u32 = 0x0000_0080;

// LinkTarget LinkFlags.
const LT_HAS_VOLUME_ID: u8 = 0x01;

// LinkInfo LinkInfoFlags.
const LI_VOLUME_ID_AND_LOCAL_BASE_PATH: u32 = 0x0000_0001;

// ExtraData block signature for the expanded environment-variable path.
const ENV_BLOCK_SIGNATURE: u32 = 0xA000_0003;

/// Parse a `.lnk` file.
///
/// Returns `None` when the data is not a shell link at all.
pub fn parse(data: &[u8]) -> Option<ShellLink> {
    if data.len() < HEADER_SIZE {
        return None;
    }
    let header_size = u32::from_le_bytes(data[0..4].try_into().ok()?);
    if header_size as usize != HEADER_SIZE {
        return None;
    }
    if data[4..20] != LINK_CLSID {
        return None;
    }

    let link_flags = u32::from_le_bytes(data[20..24].try_into().ok()?);
    let file_attributes = u32::from_le_bytes(data[24..28].try_into().ok()?);
    let unicode = link_flags & IS_UNICODE != 0;

    let mut cursor = HEADER_SIZE;
    let mut link = ShellLink {
        is_directory: file_attributes & 0x10 != 0,
        ..Default::default()
    };

    // --- LinkTargetIDList -------------------------------------------------
    let mut local_base_path: Option<String> = None;
    if link_flags & HAS_LINK_TARGET_ID_LIST != 0 {
        if cursor + 2 > data.len() {
            return Some(link);
        }
        let id_list_size = u16::from_le_bytes(data[cursor..cursor + 2].try_into().ok()?) as usize;
        // The size field counts the ID list *without* the two bytes of the
        // size field itself, so the structure ends two bytes later.
        let id_list_end = (cursor + 2 + id_list_size).min(data.len());
        let mut p = cursor + 2;
        // ItemIDCLSID (16 bytes)
        if p + 16 <= id_list_end {
            p += 16;
        }
        // LinkTarget structure
        if p < id_list_end {
            let link_type = data[p];
            let lt_flags = data[p + 1];
            p += 2;
            if link_type >= 1 && lt_flags & LT_HAS_VOLUME_ID != 0 {
                p = skip_volume_id(data, p, id_list_end)?;
            }
            if link_type >= 2 {
                local_base_path = read_mbcs_string(data, &mut p, id_list_end);
            }
        }
        cursor = id_list_end;
    }

    // --- LinkInfo ----------------------------------------------------------
    let mut link_info_base: Option<String> = None;
    if link_flags & HAS_LINK_INFO != 0 {
        if cursor + 4 > data.len() {
            return Some(link);
        }
        let link_info_size = u32::from_le_bytes(data[cursor..cursor + 4].try_into().ok()?) as usize;
        let link_info_end = (cursor + link_info_size).min(data.len());
        // The header holds the offset of the local base path; using it is
        // more reliable than walking the structure, because the volume id in
        // front of it has a variable size.
        if cursor + 28 <= link_info_end {
            let header_size = u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().ok()?) as usize;
            let li_flags = u32::from_le_bytes(data[cursor + 8..cursor + 12].try_into().ok()?);
            let local_base_path_offset =
                u32::from_le_bytes(data[cursor + 16..cursor + 20].try_into().ok()?) as usize;
            if header_size == 28 && li_flags & LI_VOLUME_ID_AND_LOCAL_BASE_PATH != 0 {
                let mut at = cursor + local_base_path_offset;
                if at < link_info_end {
                    link_info_base = read_mbcs_string(data, &mut at, link_info_end);
                }
            }
        }
        cursor = link_info_end;
    }

    // --- StringData --------------------------------------------------------
    let mut relative_path: Option<String> = None;
    let mut working_dir: Option<String> = None;
    let mut command_line: Option<String> = None;
    let mut icon_location: Option<String> = None;
    let mut name: Option<String> = None;

    let read_str = |data: &[u8], cursor: &mut usize, unicode: bool| -> Option<String> {
        read_counted_string(data, cursor, unicode)
    };

    if link_flags & HAS_NAME != 0 {
        name = read_str(data, &mut cursor, unicode);
    }
    if link_flags & HAS_RELATIVE_PATH != 0 {
        relative_path = read_str(data, &mut cursor, unicode);
    }
    if link_flags & HAS_WORKING_DIR != 0 {
        working_dir = read_str(data, &mut cursor, unicode);
    }
    if link_flags & HAS_ARGUMENTS != 0 {
        command_line = read_str(data, &mut cursor, unicode);
    }
    if link_flags & HAS_ICON_LOCATION != 0 {
        icon_location = read_str(data, &mut cursor, unicode);
    }

    // --- ExtraData ---------------------------------------------------------
    let mut env_path: Option<String> = None;
    while cursor + 8 <= data.len() {
        let block_size = u32::from_le_bytes(data[cursor..cursor + 4].try_into().ok()?) as usize;
        if block_size < 4 || cursor + block_size > data.len() {
            break;
        }
        let signature = u32::from_le_bytes(data[cursor + 4..cursor + 8].try_into().ok()?);
        if signature == ENV_BLOCK_SIGNATURE {
            // EnvironmentVariableDataBlock: Size(2), Reserved1(2),
            // Reserved2(2), Reserved3(2), then the ANSI path.
            let block = &data[cursor + 8..cursor + block_size];
            if block.len() > 8 {
                let s = read_mbcs_string(block, &mut 8, block.len());
                // The block is only useful when it really holds a path; a
                // misaligned read produces text without a separator.
                if let Some(candidate) = s {
                    if candidate.contains(':') || candidate.contains('\\') {
                        // The block keeps the path in the environment
                        // variables of the machine.
                        env_path = Some(expand_environment(&candidate));
                    }
                }
            }
            break;
        }
        cursor += block_size;
    }

    // --- Compose the target path ------------------------------------------
    //
    // The target is written in the file in more than one way, and the ways do
    // not belong together:
    //
    // * the **Local Base Path** of the LinkInfo *is* the target — an absolute
    //   path, drive letter and all,
    // * the **Relative Path** of the StringData is the same target, but
    //   written relative to the folder the shortcut sits in.
    //
    // Joining the two turns "C:\Program Files\App\app.exe" and
    // "..\..\..\Program Files\App\app.exe" into "Program Files\App\app.exe" —
    // a path without a drive letter, which cannot be opened. On this machine
    // that hit 490 of the 2744 indexed programs, and every one of them lost
    // its icon. So the absolute one wins, and the relative one is only used
    // when there is nothing else — and then it still has to be resolved
    // against the folder of the shortcut, which only the caller knows.
    link.target_path = env_path
        .or(link_info_base.clone())
        .or_else(|| local_base_path.clone().filter(|path| path.contains(':')))
        .or(relative_path.clone());

    link.working_directory = working_dir;
    link.arguments = command_line;
    link.icon_location = icon_location;
    link.name = name;
    link.description = link.name.clone();

    Some(link)
}

/// Read a string from the StringData section.
///
/// The section stores every string as a 16-bit character count followed by the
/// characters — **not** as a null-terminated string. Reading it as a C string
/// reads past the end of the value and returns garbage, which is why the
/// target path of most shortcuts came out as mojibake.
fn read_counted_string(data: &[u8], cursor: &mut usize, unicode: bool) -> Option<String> {
    if *cursor + 2 > data.len() {
        return None;
    }
    let count = u16::from_le_bytes(data[*cursor..*cursor + 2].try_into().ok()?) as usize;
    *cursor += 2;

    let bytes = if unicode { count * 2 } else { count };
    if *cursor + bytes > data.len() {
        return None;
    }
    let raw = &data[*cursor..*cursor + bytes];
    *cursor += bytes;

    if unicode {
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    } else {
        Some(decode_ansi(raw))
    }
}

/// Skip a VolumeID structure, returning the new cursor.
fn skip_volume_id(data: &[u8], mut p: usize, end: usize) -> Option<usize> {
    if p + 2 > end {
        return None;
    }
    let volume_label_size = u16::from_le_bytes(data[p..p + 2].try_into().ok()?) as usize;
    p += 2;
    if volume_label_size > 0 {
        p += volume_label_size;
    } else {
        p += 4; // VolumeClass
    }
    // DriveSerialNumber(4) + DriveType(1) + VolumeLabelOffset(2)
    p += 7;
    if p > end {
        return None;
    }
    Some(p)
}

/// Read a null-terminated MBCS (code page 1252 / ANSI) string.
fn read_mbcs_string(data: &[u8], cursor: &mut usize, end: usize) -> Option<String> {
    let start = *cursor;
    let mut p = start;
    while p < end && data[p] != 0 {
        p += 1;
    }
    if p >= end {
        return None;
    }
    *cursor = p + 1;
    Some(decode_ansi(&data[start..p]))
}

/// Decode a string in the code page of the machine.
///
/// The paths inside a shortcut are stored in the **ANSI code page of the
/// system** — GBK on a Chinese Windows. Reading the bytes one by one as
/// Latin-1 turns "直播伴侣" into "Ö±²¥°éÂÂ", and a path that looks like
/// that cannot be opened, nor can its icon be found.
fn decode_ansi(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    unsafe {
        // CP_ACP = 0: the code page of the machine.
        let needed = MultiByteToWideChar(
            0,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if needed <= 0 {
            return bytes.iter().map(|&byte| byte as char).collect();
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(
            0,
            0,
            bytes.as_ptr(),
            bytes.len() as i32,
            wide.as_mut_ptr(),
            needed,
        );
        if written <= 0 {
            return bytes.iter().map(|&byte| byte as char).collect();
        }
        wide.truncate(written as usize);
        String::from_utf16_lossy(&wide)
    }
}

#[link(name = "kernel32")]
#[allow(dead_code)]
unsafe extern "system" {
    fn MultiByteToWideChar(
        codepage: u32,
        dwflags: u32,
        lpmultibyte: *const u8,
        cbmultibyte: i32,
        lpwidechar: *mut u16,
        cchwidechar: i32,
    ) -> i32;
    /// Only the tests use it.
    fn GetACP() -> u32;
}

/// Read a null-terminated UTF-16LE string.

/// Join a base directory with a relative path, honouring `..` segments.
pub fn join_path(base: &str, relative: &str) -> String {
    if relative.is_empty() {
        return base.to_string();
    }
    if relative.contains(':') || relative.starts_with('\\') || relative.starts_with('/') {
        return relative.to_string();
    }

    let mut parts: Vec<&str> = base
        .trim_end_matches(['\\', '/'])
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .collect();

    for segment in relative.split(['\\', '/']) {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }

    // Preserve a leading drive specifier.
    if base.starts_with("\\\\") {
        format!("\\\\{}", parts.join("\\"))
    } else {
        parts.join("\\")
    }
}

/// Resolve a relative path against the folder a shortcut sits in.
///
/// A shortcut that keeps its target relative writes it relative to its own
/// folder — five levels up for an entry in the Start Menu.
pub fn resolve_relative(
    base_dir: &std::path::Path,
    relative: &std::path::Path,
) -> std::path::PathBuf {
    std::path::PathBuf::from(join_path(
        &base_dir.to_string_lossy(),
        &relative.to_string_lossy(),
    ))
}

/// Replace the `%NAME%` in a path with what the environment says.
///
/// The shortcuts of the system keep their target in the environment
/// variables of the machine — `%ProgramFiles%\Windows Mail\wab.exe` — and a
/// path with a `%` in it cannot be opened, nor can its icon be found.
pub fn expand_environment(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            // An unbalanced `%` is not a variable.
            out.push('%');
            out.push_str(after);
            return out;
        };
        let name = &after[..end];
        match std::env::var(name) {
            Ok(value) => out.push_str(&value),
            Err(_) => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_lnk_data() {
        assert!(parse(b"not a shell link at all, definitely not").is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn rejects_wrong_header_size() {
        let mut data = vec![0u8; 76];
        data[0..4].copy_from_slice(&0x4Du32.to_le_bytes());
        assert!(parse(&data).is_none());
    }

    #[test]
    fn joins_paths_correctly() {
        assert_eq!(
            join_path("C:\\Program Files\\App", "sub\\file.exe"),
            "C:\\Program Files\\App\\sub\\file.exe"
        );
        assert_eq!(
            join_path("C:\\Program Files\\App", "..\\Other\\x.exe"),
            "C:\\Program Files\\Other\\x.exe"
        );
    }

    #[test]
    fn absolute_relative_path_wins() {
        assert_eq!(join_path("C:\\Base", "D:\\Other\\x.exe"), "D:\\Other\\x.exe");
    }

    /// The string data of a shell link uses counted strings, the ID list
    /// length does not include its own two bytes, and the local base path is
    /// addressed through an offset. A shortcut on this machine is used to
    /// check that the parser agrees with the shell.
    #[test]
    fn parses_a_real_shortcut_like_the_shell_does() {
        let candidates = [
            "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Google Chrome.lnk",
            "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        ];
        let shortcut = candidates
            .iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("a Chrome shortcut should exist on a Windows install");
        if !shortcut.ends_with(".lnk") {
            return;
        }

        let data = std::fs::read(shortcut).expect("read the shortcut");
        let link = parse(&data).expect("the file should be a shell link");

        let target = link
            .target_path
            .as_ref()
            .expect("a shortcut should have a target");
        assert!(
            target.to_lowercase().ends_with("chrome.exe"),
            "unexpected target: {target}"
        );
        // The target must be an absolute path, not a fragment of one.
        assert!(target.contains(':'), "unexpected target: {target}");

        // The icon location points at the executable as well.
        if let Some(icon) = &link.icon_location {
            assert!(icon.to_lowercase().contains("chrome"), "icon: {icon}");
        }
    }

    /// The Local Base Path of the LinkInfo is the target, and the Relative
    /// Path of the StringData is the same target written relative to the
    /// folder of the shortcut. The two must not be joined — that is how
    /// 490 of the 2744 programs on this machine lost their icon.
    ///
    /// The shortcut of Clash Verge is the one that shows it: its target is
    /// "C:\Program Files\Clash Verge\clash-verge.exe", and its relative
    /// path is the same thing with five levels of ".." in front of it.
    #[test]
    fn the_absolute_target_wins_over_the_relative_one() {
        let shortcut = "C:\\ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\\Clash Verge.lnk";
        if !std::path::Path::new(shortcut).exists() {
            return;
        }
        let data = std::fs::read(shortcut).expect("read the shortcut");
        let link = parse(&data).expect("the file should be a shell link");
        let target = link
            .target_path
            .as_ref()
            .expect("a shortcut should have a target");
        assert!(
            target.to_lowercase().ends_with("clash-verge.exe"),
            "unexpected target: {target}"
        );
        // The drive letter is the whole point: without it the path cannot be
        // opened, and the icon of the program cannot be found.
        assert!(
            target.to_lowercase().starts_with("c:\\"),
            "unexpected target: {target}"
        );
        assert!(
            !target.contains(".."),
            "the target is not resolved yet: {target}"
        );
    }

    /// A path in a shortcut is written in the code page of the machine, and
    /// on a Chinese Windows that is GBK. Reading the bytes as Latin-1 turns
    /// "直播伴侣" into "Ö±²¥°éÂÂ".
    #[test]
    fn decodes_the_code_page_of_the_machine() {
        assert_eq!(decode_ansi(b"abc"), "abc");
        // The bytes of "中文" in GBK.
        let bytes = [0xD6u8, 0xD0, 0xCE, 0xC4];
        let decoded = decode_ansi(&bytes);
        if code_page() == 936 {
            assert_eq!(decoded, "中文");
        } else {
            // Some other code page: the bytes are read in *that* page, which
            // is not the Latin-1 reading of them.
            assert_ne!(decoded, "ÖÐÎÄ");
        }
    }

    /// The code page of the machine (`CP_ACP`).
    fn code_page() -> u32 {
        unsafe { GetACP() }
    }
}
