//! The details of a file, for the preview pane.
//!
//! Everything collected here is cheap: a stat, a handful of questions to the
//! shell, the registry and the file system. **The content of the file is
//! never read** — a four gigabyte video must not be read to say how big it
//! is, and a disk that is slow to wake up must not hold the window back.
//!
//! The work happens on a background thread: the pane asks for the details of
//! the selected row, gets what is there, and the rest arrives a moment
//! later.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;

use crate::winapi::*;

/// The file attributes of Windows.
mod attribute {
    pub const READONLY: u32 = 0x0000_0001;
    pub const HIDDEN: u32 = 0x0000_0002;
    pub const SYSTEM: u32 = 0x0000_0004;
    pub const ARCHIVE: u32 = 0x0000_0020;
    pub const TEMPORARY: u32 = 0x0000_0100;
    pub const SPARSE_FILE: u32 = 0x0000_0200;
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    pub const COMPRESSED: u32 = 0x0000_0800;
    pub const OFFLINE: u32 = 0x0000_1000;
    pub const ENCRYPTED: u32 = 0x0000_4000;
    pub const INTEGRITY_STREAM: u32 = 0x0000_8000;
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
}

/// How many children of a folder are counted before the count gives up.
///
/// A folder with a hundred thousand files is not something to count for a
/// preview.
const CHILD_LIMIT: u64 = 2000;

/// How many details are kept at the same time.
const CACHE_SIZE: usize = 64;

/// The details of one file or folder.
#[derive(Debug, Clone, Default)]
pub struct FileDetails {
    /// The full path.
    pub path: String,
    /// The name.
    pub name: String,
    /// The name of the folder the file is in.
    pub folder: String,
    /// The kind of the file, as the shell calls it.
    pub kind: String,
    /// The length of the content, in bytes.
    pub size: Option<u64>,
    /// How much room the file takes on the disk.
    pub size_on_disk: Option<u64>,
    /// When the file was created.
    pub created: Option<SystemTime>,
    /// When the file was written to.
    pub modified: Option<SystemTime>,
    /// When the file was read.
    pub accessed: Option<SystemTime>,
    /// The attribute marks, in the words of the file manager.
    pub attributes: Vec<String>,
    /// How many names point at the file.
    pub links: Option<u32>,
    /// The number the file system keeps the file under.
    pub file_index: Option<u64>,
    /// The volume the file is on.
    pub volume: Option<VolumeInfo>,
    /// The program that opens this kind of file.
    pub opener: Option<String>,
    /// The content type of the registry.
    pub mime: Option<String>,
    /// The alternate data streams of the file.
    pub streams: Vec<String>,
    /// Where the file came from, when it says so.
    pub zone: Option<String>,
    /// Where a link points to.
    pub link_target: Option<String>,
    /// How many things a folder holds.
    pub children: Option<u64>,
    /// The picture of an image file.
    pub image: Option<ImageInfo>,
    /// What is known about a video file.
    pub video: Option<VideoInfo>,
    /// The extra rows the shell knows about the file.
    pub shell: Vec<(String, String)>,
}

/// The volume a file is on.
#[derive(Debug, Clone, Default)]
pub struct VolumeInfo {
    /// The label of the volume, the name Windows shows.
    pub label: String,
    /// The kind of the file system: NTFS, exFAT…
    pub file_system: String,
    /// The serial number of the volume.
    pub serial: Option<u32>,
    /// How much room is left.
    pub free: Option<u64>,
    /// How big the volume is.
    pub total: Option<u64>,
}

/// The picture of an image file.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// Whether the picture has a transparent channel.
    pub has_alpha: bool,
}

/// What is known about a video file.
///
/// The video module is the one that asks the file about itself, so that
/// there is only one answer to "how long is this video".
pub type VideoInfo = crate::video::VideoInfo;

impl FileDetails {
    /// The rows of the panel, grouped.
    ///
    /// The groups are what the pane draws; the details themselves are put
    /// in the group where they belong.
    pub fn groups(&self) -> Vec<(&'static str, Vec<(&str, String)>)> {
        let mut groups: Vec<(&'static str, Vec<(&str, String)>)> = Vec::new();

        let mut basic: Vec<(&str, String)> = Vec::new();
        if !self.kind.is_empty() {
            basic.push(("类型", self.kind.clone()));
        }
        if let Some(size) = self.size {
            basic.push(("大小", format!("{} ({} 字节)", human_size(size), size)));
        }
        if let Some(on_disk) = self.size_on_disk {
            // A folder takes no room of its own — saying "0 B" is noise.
            if Some(on_disk) != self.size && on_disk > 0 {
                basic.push(("占用空间", human_size(on_disk)));
            }
        }
        if !self.folder.is_empty() {
            basic.push(("位置", self.folder.clone()));
        }
        if let Some(children) = self.children {
            basic.push(("包含", format!("{children} 个项目")));
        }
        if let Some(index) = self.file_index {
            basic.push(("索引号", index.to_string()));
        }
        if let Some(links) = self.links {
            if links > 1 {
                basic.push(("硬链接", links.to_string()));
            }
        }
        if !groups.is_empty() || !basic.is_empty() {
            groups.push(("基本信息", basic));
        }

        let mut times: Vec<(&str, String)> = Vec::new();
        if let Some(time) = self.created {
            times.push(("创建时间", format_time(time)));
        }
        if let Some(time) = self.modified {
            times.push(("修改时间", format_time(time)));
        }
        if let Some(time) = self.accessed {
            times.push(("访问时间", format_time(time)));
        }
        if !times.is_empty() {
            groups.push(("时间", times));
        }

        let mut marks: Vec<(&str, String)> = Vec::new();
        if !self.attributes.is_empty() {
            marks.push(("属性", self.attributes.join("、")));
        }
        if let Some(target) = &self.link_target {
            marks.push(("链接指向", target.clone()));
        }
        if let Some(zone) = &self.zone {
            marks.push(("来源", zone.clone()));
        }
        if !self.streams.is_empty() {
            marks.push(("数据流", self.streams.join("、")));
        }
        if !marks.is_empty() {
            groups.push(("属性与来源", marks));
        }

        let mut media: Vec<(&str, String)> = Vec::new();
        if let Some(image) = &self.image {
            media.push((
                "尺寸",
                format!("{} × {} 像素", image.width, image.height),
            ));
            if image.has_alpha {
                media.push(("色彩", "带透明通道".to_owned()));
            }
        }
        if let Some(video) = &self.video {
            if let Some(duration) = video.duration {
                media.push(("时长", format_duration(duration)));
            }
            if video.width > 0 && video.height > 0 {
                media.push(("分辨率", format!("{} × {}", video.width, video.height)));
            }
            if let Some(rate) = video.frame_rate {
                media.push(("帧率", format!("{rate:.2} fps")));
            }
            if let Some(codec) = &video.codec {
                media.push(("编码", codec.clone()));
            }
        }
        if !media.is_empty() {
            groups.push(("媒体", media));
        }

        // What the shell knows beyond the file system: the codec of a video,
        // the camera that took a picture, the author of a document.
        if !self.shell.is_empty() {
            let rows: Vec<(&str, String)> = self
                .shell
                .iter()
                .map(|(label, value)| (label.as_str(), value.clone()))
                .collect();
            groups.push(("更多", rows));
        }

        let mut related: Vec<(&str, String)> = Vec::new();
        if let Some(opener) = &self.opener {
            related.push(("打开方式", opener.clone()));
        }
        if let Some(mime) = &self.mime {
            related.push(("内容类型", mime.clone()));
        }
        if !related.is_empty() {
            groups.push(("关联", related));
        }

        let mut storage: Vec<(&str, String)> = Vec::new();
        if let Some(volume) = &self.volume {
            if !volume.label.is_empty() {
                storage.push(("卷标", volume.label.clone()));
            }
            if !volume.file_system.is_empty() {
                storage.push(("文件系统", volume.file_system.clone()));
            }
            if let Some(serial) = volume.serial {
                storage.push(("卷序列号", format!("{serial:08X}")));
            }
            if let Some(free) = volume.free {
                storage.push(("可用空间", human_size(free)));
            }
            if let Some(total) = volume.total {
                storage.push(("总容量", human_size(total)));
            }
        }
        if !storage.is_empty() {
            groups.push(("存储", storage));
        }

        groups
    }
}

/// Collect everything that is cheap to know about a file.
pub fn collect(path: &Path) -> FileDetails {
    let mut details = FileDetails {
        path: path.to_string_lossy().to_string(),
        ..Default::default()
    };
    details.name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| details.path.clone());
    details.folder = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // ---- the stat ------------------------------------------------------
    let metadata = std::fs::metadata(path).ok();
    if let Some(metadata) = &metadata {
        details.size = Some(metadata.len());
        details.created = metadata.created().ok();
        details.modified = metadata.modified().ok();
        details.accessed = metadata.accessed().ok();
        let flags = metadata.file_attributes();
        let mut marks = Vec::new();
        if flags & attribute::READONLY != 0 {
            marks.push("只读".to_owned());
        }
        if flags & attribute::HIDDEN != 0 {
            marks.push("隐藏".to_owned());
        }
        if flags & attribute::SYSTEM != 0 {
            marks.push("系统".to_owned());
        }
        if flags & attribute::ARCHIVE != 0 {
            marks.push("存档".to_owned());
        }
        if flags & attribute::TEMPORARY != 0 {
            marks.push("临时".to_owned());
        }
        if flags & attribute::SPARSE_FILE != 0 {
            marks.push("稀疏".to_owned());
        }
        if flags & attribute::REPARSE_POINT != 0 {
            marks.push("重解析点".to_owned());
        }
        if flags & attribute::COMPRESSED != 0 {
            marks.push("压缩".to_owned());
        }
        if flags & attribute::OFFLINE != 0 {
            marks.push("脱机".to_owned());
        }
        if flags & attribute::ENCRYPTED != 0 {
            marks.push("加密".to_owned());
        }
        if flags & attribute::INTEGRITY_STREAM != 0 {
            marks.push("完整性流".to_owned());
        }
        if flags & attribute::RECALL_ON_OPEN != 0 {
            marks.push("远程".to_owned());
        }
        details.attributes = marks;
    }
    // ---- the kind, as the shell calls it -------------------------------
    details.kind = shell_type_name(path);
    // ---- the room the file takes ---------------------------------------
    details.size_on_disk = compressed_size(path);
    // ---- the identity of the file --------------------------------------
    if let Some((links, index)) = file_identity(path) {
        details.links = Some(links);
        details.file_index = Some(index);
    }
    // ---- the volume ----------------------------------------------------
    if let Some(root) = volume_root(path) {
        details.volume = volume_info(&root);
    }
    // ---- what opens it -------------------------------------------------
    if let Some(extension) = path.extension().map(|e| e.to_string_lossy().to_string()) {
        details.opener = opener_for(&extension);
        details.mime = content_type_for(&extension);
    }
    // ---- the streams ---------------------------------------------------
    details.streams = streams_of(path);
    details.zone = zone_of(path);
    // ---- where a link points --------------------------------------------
    if let Ok(target) = std::fs::read_link(path) {
        details.link_target = Some(target.to_string_lossy().to_string());
    }
    // ---- how much a folder holds ---------------------------------------
    if let Some(metadata) = &metadata {
        if metadata.is_dir() {
            details.children = count_children(path);
        }
    }
    // ---- what kind of picture or video it is ---------------------------
    details.image = image_info(path);
    // The file manager knows more than the file system does: how long a
    // video runs, which camera took a picture.
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let maybe_video = crate::video::is_video_extension(&extension);
    // Only a file that *can* be a video is asked of FFmpeg. The pointer
    // chooses the row it is over, so every row it passes brings its details
    // in — and asking every file would mean starting a program for each of
    // them, over and over, for files that are not videos at all.
    details.video = if maybe_video {
        crate::video::probe(path)
    } else {
        None
    };
    // ---- what the shell knows about it ---------------------------------
    let wanted: &[&str] = if maybe_video {
        crate::props::VIDEO_PROPERTIES
    } else if is_image_extension(&extension) {
        crate::props::IMAGE_PROPERTIES
    } else {
        crate::props::FILE_PROPERTIES
    };
    let properties = crate::props::read(path, wanted);
    for (name, value) in properties.values {
        let Some(value) = value else { continue };
        // The length of a video is asked of the shell, because the decoder
        // does not know it.
        if name == "System.Media.Duration" {
            if let Ok(millis) = value.parse::<u64>() {
                if let Some(video) = details.video.as_mut() {
                    video.duration = Some(millis / 10_000);
                }
            }
            continue;
        }
        let label = property_label(&name);
        let value = property_value(&name, &value);
        details.shell.push((label, value));
    }

    details
}

/// The name a property is shown under.
fn property_label(name: &str) -> String {
    match name {
        "System.Video.FrameWidth" => "帧宽度".to_owned(),
        "System.Video.FrameHeight" => "帧高度".to_owned(),
        "System.Video.FrameRate" => "帧速率".to_owned(),
        "System.Video.EncodingBitrate" => "编码比特率".to_owned(),
        "System.Video.DataRate" => "数据速率".to_owned(),
        "System.Video.Compression" => "视频压缩".to_owned(),
        "System.Media.DateEncoded" => "媒体创建日期".to_owned(),
        "System.Photo.PixelWidth" => "像素宽度".to_owned(),
        "System.Photo.PixelHeight" => "像素高度".to_owned(),
        "System.Photo.CameraMake" => "相机厂商".to_owned(),
        "System.Photo.CameraModel" => "相机型号".to_owned(),
        "System.Photo.DateTaken" => "拍摄时间".to_owned(),
        "System.Photo.ExposureTime" => "曝光时间".to_owned(),
        "System.Photo.FNumber" => "光圈".to_owned(),
        "System.Photo.ISOSpeed" => "感光度".to_owned(),
        "System.Photo.Orientation" => "方向".to_owned(),
        "System.Search.Keywords" => "关键词".to_owned(),
        "System.Comment" => "备注".to_owned(),
        "System.Author" => "作者".to_owned(),
        "System.Title" => "标题".to_owned(),
        "System.Subject" => "主题".to_owned(),
        "System.Category" => "类别".to_owned(),
        _ => name.to_owned(),
    }
}

/// The value of a property, in words.
fn property_value(name: &str, value: &str) -> String {
    match name {
        // The frame rate is in frames per thousand seconds.
        "System.Video.FrameRate" => match value.parse::<u64>() {
            Ok(rate) => format!("{:.2} 帧/秒", rate as f64 / 1000.0),
            Err(_) => value.to_owned(),
        },
        // The bitrates are in bits per second.
        "System.Video.EncodingBitrate" | "System.Video.DataRate" => {
            match value.parse::<u64>() {
                Ok(bits) => format!("{}/s", human_size(bits / 8)),
                Err(_) => value.to_owned(),
            }
        }
        // The compression is the four letters of the codec.
        "System.Video.Compression" => codec_name(value),
        "System.Photo.ISOSpeed" => format!("ISO {value}"),
        _ => value.to_owned(),
    }
}

/// The name of a codec, from the four letters the shell keeps.
fn codec_name(value: &str) -> String {
    let letters = value
        .trim_start_matches('{')
        .split('-')
        .next()
        .and_then(|part| u32::from_str_radix(part, 16).ok())
        .map(|number| {
            let bytes = number.to_le_bytes();
            String::from_utf8_lossy(&bytes).to_string()
        })
        .unwrap_or_default();
    match letters.as_str() {
        "H264" | "AVC1" | "DAVC" => "H.264 / AVC".to_owned(),
        "HEVC" | "HVC1" => "H.265 / HEVC".to_owned(),
        "VP80" => "VP8".to_owned(),
        "VP90" => "VP9".to_owned(),
        "AV01" => "AV1".to_owned(),
        "MP4V" => "MPEG-4 视频".to_owned(),
        "MJPG" => "Motion JPEG".to_owned(),
        "WMV3" => "Windows Media Video".to_owned(),
        "DIVX" => "DivX".to_owned(),
        "" => value.to_owned(),
        other => other.to_owned(),
    }
}

/// The kind of the file, in the words of the shell.
fn shell_type_name(path: &Path) -> String {
    let wide = wide_of(path);
    if wide.is_empty() {
        return String::new();
    }
    unsafe {
        let mut info: SHFILEINFO = std::mem::zeroed();
        let ok = SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFO>() as u32,
            SHGFI_TYPENAME,
        );
        if ok == 0 {
            return String::new();
        }
        let text: Vec<u16> = info.szTypeName.iter().copied().take_while(|c| *c != 0).collect();
        String::from_utf16_lossy(&text)
    }
}

/// How much room the file takes on the disk.
fn compressed_size(path: &Path) -> Option<u64> {
    let wide = wide_of(path);
    if wide.is_empty() {
        return None;
    }
    unsafe {
        let mut high: u32 = 0;
        let low = GetCompressedFileSizeW(wide.as_ptr(), &mut high);
        // When the call fails it answers with the error code, which is not a
        // size at all.
        if low == u32::MAX && high == u32::MAX {
            return None;
        }
        Some(((high as u64) << 32) | low as u64)
    }
}

/// How many names point at the file, and the number the file system keeps
/// it under.
fn file_identity(path: &Path) -> Option<(u32, u64)> {
    let wide = wide_of(path);
    if wide.is_empty() {
        return None;
    }
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let ok = GetFileInformationByHandle(handle, &mut info);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Some((info.nNumberOfLinks, index))
    }
}

/// The root of the volume a path is on.
fn volume_root(path: &Path) -> Option<String> {
    let text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix("\\\\") {
        // A network path: \\server\share\…
        let mut parts = rest.split(['\\', '/']).filter(|s| !s.is_empty());
        let server = parts.next()?;
        let share = parts.next()?;
        return Some(format!("\\\\{server}\\{share}"));
    }
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        // The root of a volume is "C:\", with the backslash — the volume
        // information does not answer for a bare "C:".
        return Some(text[..2].to_string() + "\\");
    }
    None
}

/// The label, the kind and the room of a volume.
fn volume_info(root: &str) -> Option<VolumeInfo> {
    let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut label = vec![0u16; 260];
        let mut system = vec![0u16; 260];
        let mut serial: u32 = 0;
        let mut maximum: u32 = 0;
        let mut flags: u32 = 0;
        let ok = GetVolumeInformationW(
            wide.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut maximum,
            &mut flags,
            system.as_mut_ptr(),
            system.len() as u32,
        );
        if ok == 0 {
            return None;
        }
        let mut info = VolumeInfo {
            label: from_wide(&label),
            file_system: from_wide(&system),
            serial: Some(serial),
            ..Default::default()
        };
        let mut free = 0u64;
        let mut total = 0u64;
        let mut to_caller = 0u64;
        if GetDiskFreeSpaceExW(wide.as_ptr(), &mut to_caller, &mut total, &mut free) != 0 {
            info.free = Some(free);
            info.total = Some(total);
        }
        Some(info)
    }
}

/// The program that opens a kind of file.
///
/// The registry says which kind of file it is (`HKCR\.txt` → "txtfilelegacy")
/// and that kind says which program opens it (`HKCR\txtfilelegacy\shell\open\
/// command`).
fn opener_for(extension: &str) -> Option<String> {
    let prog_id = registry_string(&format!(".{extension}"))?;
    if prog_id.is_empty() {
        return None;
    }
    let command = registry_string(&format!("{prog_id}\\shell\\open\\command"))?;
    if command.is_empty() {
        return None;
    }
    // The command looks like "C:\path\app.exe "%1"" — the program is what
    // comes before the first quote, or the part between the first pair of
    // quotes when the program is quoted itself.
    let program = match command.find('"') {
        Some(at) => {
            let before = command[..at].trim();
            if !before.is_empty() {
                before.to_owned()
            } else {
                command.split('"').nth(1).unwrap_or_default().trim().to_owned()
            }
        }
        None => command.split_whitespace().next().unwrap_or_default().to_owned(),
    };
    if program.is_empty() {
        None
    } else {
        Some(program)
    }
}

/// The default value of a key in the classes of the files.
fn registry_string(subkey: &str) -> Option<String> {
    let subkey: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    // The default value of a key has no name at all.
    let value: Vec<u16> = vec![0u16];
    let mut kind: u32 = 0;
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CLASSES_ROOT,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize / 2) + 1];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CLASSES_ROOT,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut kind,
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let text = from_wide(&buffer);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The content type the registry keeps for a kind of file.
fn content_type_for(extension: &str) -> Option<String> {
    let subkey: Vec<u16> = format!(".{extension}\\Content Type")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // The default value of a key has no name at all — "(Default)" is how the
    // registry editor shows it, not how it is asked for.
    let value: Vec<u16> = vec![0u16];
    let mut kind: u32 = 0;
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CLASSES_ROOT,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut kind,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if status != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u16; (size as usize / 2) + 1];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CLASSES_ROOT,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            &mut kind,
            buffer.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let text = from_wide(&buffer);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The alternate data streams of a file.
fn streams_of(path: &Path) -> Vec<String> {
    let wide = wide_of(path);
    if wide.is_empty() {
        return Vec::new();
    }
    let mut names = Vec::new();
    unsafe {
        let mut data: WIN32_FIND_STREAM_DATA = std::mem::zeroed();
        let handle = FindFirstStreamW(wide.as_ptr(), 0, &mut data, 0);
        if handle == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        loop {
            let name: Vec<u16> = data
                .cStreamName
                .iter()
                .copied()
                .take_while(|c| *c != 0)
                .collect();
            let name = String::from_utf16_lossy(&name);
            // The content itself is not a stream.
            if name != "::$DATA" && !name.is_empty() {
                names.push(name);
            }
            if FindNextStreamW(handle, &mut data) == 0 {
                break;
            }
        }
        FindClose(handle);
    }
    names
}

/// Where the file came from, when it says so.
///
/// Windows writes a stream called `Zone.Identifier` on a file that comes
/// from the internet or from a downloaded archive.
fn zone_of(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let mut stream = path.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    let wide: Vec<u16> = stream.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let text = std::fs::read_to_string(std::path::PathBuf::from(
        String::from_utf16_lossy(&wide),
    ))
    .ok()?;
    let mut host = None;
    let mut zone = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("HostUrl=") {
            host = Some(value.trim().to_owned());
        } else if let Some(value) = line.strip_prefix("ZoneId=") {
            zone = Some(value.trim().to_owned());
        }
    }
    let zone = zone?;
    let name = match zone.as_str() {
        "0" => "本地计算机",
        "1" => "受信任的站点",
        "2" => "受信任的站点",
        "3" => "不受信任的站点",
        "4" => "不受信任的站点",
        _ => "不受信任的站点",
    };
    match host {
        Some(host) => Some(format!("{name}({host})")),
        None => Some(name.to_owned()),
    }
}

/// How many things a folder holds.
fn count_children(path: &Path) -> Option<u64> {
    let mut count = 0u64;
    let entries = std::fs::read_dir(path).ok()?;
    for _ in entries.flatten() {
        count += 1;
        if count >= CHILD_LIMIT {
            break;
        }
    }
    Some(count)
}

/// The size of a picture, without reading the picture.
fn image_info(path: &Path) -> Option<ImageInfo> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !is_image_extension(&extension) {
        return None;
    }
    let (width, height) = image::image_dimensions(path).ok()?;
    Some(ImageInfo {
        width,
        height,
        has_alpha: matches!(
            extension.as_str(),
            "png" | "gif" | "webp" | "ico" | "tif" | "tiff"
        ),
    })
}

/// Whether the extension belongs to a picture.
fn is_image_extension(extension: &str) -> bool {
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "svg" | "heic"
            | "avif" | "jfif"
    )
}

/// The path as a wide string, with the terminator.
fn wide_of(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A wide string as text.
fn from_wide(buffer: &[u16]) -> String {
    let text: Vec<u16> = buffer.iter().copied().take_while(|c| *c != 0).collect();
    String::from_utf16_lossy(&text)
}

/// A byte count the way a file manager shows it.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// A timestamp as `YYYY-MM-DD HH:MM:SS`.
pub fn format_time(time: SystemTime) -> String {
    let Ok(duration) = time.duration_since(SystemTime::UNIX_EPOCH) else {
        return String::new();
    };
    let seconds = duration.as_secs() as i64;
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_unix(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// A duration as `1:23:45` or `23:45`.
pub fn format_duration(milliseconds: u64) -> String {
    let total = milliseconds / 1000;
    let seconds = total % 60;
    let minutes = (total / 60) % 60;
    let hours = total / 3600;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// The civil date from a number of days since the Unix epoch.
fn civil_from_unix(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A cache of the details of the files that were looked at.
pub struct DetailsCache {
    entries: Arc<Mutex<HashMap<String, Option<Arc<FileDetails>>>>>,
    /// Paths whose details are being collected.
    pending: Arc<Mutex<HashSet<String>>>,
    /// The queue the collector thread reads from.
    sender: SyncSender<String>,
}

impl DetailsCache {
    /// Create a cache and the thread that fills it.
    pub fn new(ctx: egui::Context) -> Self {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        // The queue is short on purpose: when the user moves through the list
        // quickly, the details of the rows that are gone are not worth
        // collecting.
        let (sender, receiver) = std::sync::mpsc::sync_channel::<String>(8);

        {
            let entries = entries.clone();
            let pending = pending.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("details-collector".into())
                .spawn(move || {
                    while let Ok(path) = receiver.recv() {
                        let details = {
                            let _timer = launcher_core::profile::phase("preview.details");
                            collect(Path::new(&path))
                        };
                        let key = path.to_ascii_lowercase();
                        let mut entries = entries.lock();
                        if entries.len() > CACHE_SIZE {
                            entries.clear();
                        }
                        entries.insert(key.clone(), Some(Arc::new(details)));
                        drop(entries);
                        pending.lock().remove(&key);
                        ctx.request_repaint();
                    }
                })
                .ok();
        }

        Self {
            entries,
            pending,
            sender,
        }
    }

    /// The details of a file, **only if they are already there**.
    pub fn get_cached(&self, path: &Path) -> Option<Arc<FileDetails>> {
        let key = path.to_string_lossy().to_ascii_lowercase();
        self.entries.lock().get(&key).cloned().flatten()
    }

    /// The details of a file, asking for them when they are not there.
    ///
    /// Returns `None` until the background thread has them.
    pub fn get(&self, path: &Path) -> Option<Arc<FileDetails>> {
        if let Some(hit) = self.get_cached(path) {
            return Some(hit);
        }
        let key = path.to_string_lossy().to_ascii_lowercase();
        let mut pending = self.pending.lock();
        if pending.insert(key.clone()) {
            let text = path.to_string_lossy().to_string();
            match self.sender.try_send(text) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // The queue is full: the details are wanted later, when
                    // the user stops moving through the list.
                    pending.remove(&key);
                }
                Err(TrySendError::Disconnected(_)) => {
                    pending.remove(&key);
                }
            }
        }
        drop(pending);
        // No repaint is asked for here: the thread that reads the details
        // asks for one when it is done, and that is the frame in which they
        // appear. A repaint asked for while the window is being painted is an
        // immediate one, and it makes the window draw as fast as it can.
        None
    }
}

/// The path of the file a result points at.
pub fn path_of(result: &launcher_core::result::Result) -> Option<PathBuf> {
    let text = if result.copy_text.is_empty() {
        result.subtitle.as_str()
    } else {
        result.copy_text.as_str()
    };
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}
