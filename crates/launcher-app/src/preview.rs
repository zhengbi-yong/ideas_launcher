//! The preview pane.
//!
//! Flow Launcher shows a pane next to the result list with a preview of the
//! selected row: the description a plugin sent with the result, the details
//! of a file, or a thumbnail of an image.
//!
//! The original renders plugin descriptions in a WebView, so a plugin can
//! send HTML. This launcher has no web view; the description is rendered as
//! markdown (see [`crate::markdown`]) or as plain text.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use egui::{Color32, FontId, Rect, RichText, Sense, Stroke, TextStyle, Ui, Vec2};
use launcher_core::result::{PreviewContentType, PreviewData, Result as LauncherResult};
use parking_lot::Mutex;

use crate::details::DetailsCache;
use crate::icons::IconImage;
use crate::markdown::{self, Block, Span, SpanStyle};
use crate::theme::Palette;
use crate::video::VideoPlayer;

/// The widest the preview pane may become.
pub const PREVIEW_WIDTH: f32 = 268.0;

/// The smallest window width that still has room for the preview pane.
pub const PREVIEW_MIN_WINDOW_WIDTH: f32 = 560.0;

/// The height the window keeps when a preview is on the screen.
///
/// A picture, a video with its controls and the details below them need
/// more room than a list of two rows.
pub const PREVIEW_WINDOW_HEIGHT: f32 = 470.0;

/// The longest edge of a decoded thumbnail.
const THUMBNAIL_SIZE: u32 = 320;

/// What the preview pane shows for a result.
#[derive(Debug, Clone)]
pub enum PreviewContent {
    /// Text the plugin sent with the result.
    Text {
        title: String,
        body: String,
        /// Whether the body is markdown.
        markdown: bool,
    },
    /// A file, with what the index already knows about it.
    File {
        path: String,
        name: String,
        /// The picture of an image file.
        image: Option<PathBuf>,
        /// The video of a video file.
        video: bool,
        /// The size of the file, in bytes — from the index, free.
        size: Option<u64>,
        /// When the file was last changed — from the index, free.
        modified: Option<u64>,
    },
    /// An image.
    Image { path: String, caption: String },
}

/// Work out what to show for a result.
pub fn derive(result: &LauncherResult) -> Option<PreviewContent> {
    if let Some(preview) = &result.preview {
        return from_preview_data(result, preview);
    }

    if result.plugin_id == "file" {
        let path = if result.copy_text.is_empty() {
            result.subtitle.as_str()
        } else {
            result.copy_text.as_str()
        };
        return file_content(path, result.size, result.modified);
    }

    None
}

fn from_preview_data(result: &LauncherResult, preview: &PreviewData) -> Option<PreviewContent> {
    if !preview.description.is_empty() {
        return Some(PreviewContent::Text {
            title: result.title.clone(),
            body: preview.description.clone(),
            markdown: preview.content_type == PreviewContentType::Markdown,
        });
    }

    if let Some(image_path) = &preview.image_path {
        return Some(PreviewContent::Image {
            path: image_path.clone(),
            caption: result.subtitle.clone(),
        });
    }

    if let Some(file_path) = &preview.file_path {
        return file_content(file_path, result.size, result.modified);
    }

    None
}

fn file_content(
    path: &str,
    size: Option<u64>,
    modified: Option<u64>,
) -> Option<PreviewContent> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    let file_path = PathBuf::from(path);
    let name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_owned());

    // A picture is shown in the pane, a video gets a player.
    let extension = file_path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let image = if is_image(&extension) {
        Some(file_path.clone())
    } else {
        None
    };
    let video = crate::video::is_video_extension(&extension);

    Some(PreviewContent::File {
        path: path.to_owned(),
        name,
        image,
        video,
        size,
        modified,
    })
}

/// Whether the extension belongs to a picture.
fn is_image(extension: &str) -> bool {
    matches!(
        extension,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "svg" | "heic"
            | "avif" | "jfif"
    )
}

/// Format a byte count the way a file manager does.
pub fn format_size(bytes: u64) -> String {
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

/// Format a timestamp as `YYYY-MM-DD HH:MM`.
pub fn format_time(time: Option<std::time::SystemTime>) -> String {
    let Some(time) = time else {
        return String::new();
    };
    let Ok(duration) = time.duration_since(std::time::UNIX_EPOCH) else {
        return String::new();
    };
    format_unix(duration.as_secs() as i64)
}

/// Format a number of seconds since the Unix epoch as `YYYY-MM-DD HH:MM`.
///
/// The index gives the time of a file as a number of seconds since the Unix
/// epoch, so no file has to be opened to know when it was changed.
fn format_unix(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_unix(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        rest / 3600,
        (rest % 3600) / 60
    )
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

/// A cache of decoded thumbnails.
///
/// How many thumbnails are kept.
///
/// A thumbnail is a picture of a few hundred kilobytes, and a long walk
/// through the folders would pile them up for ever.
const THUMBNAIL_LIMIT: usize = 64;

/// Decoding an image is file and CPU work, so it happens on a background
/// thread; the pane shows a placeholder until the pixels are there.
pub struct ThumbnailCache {
    entries: Arc<Mutex<HashMap<String, Option<Arc<IconImage>>>>>,
    /// The order in which the thumbnails came in, for the eviction.
    ///
    /// It is only written to — the eviction happens in the thread that decodes
    /// — but it has to be kept here, or the thread would not be able to reach it.
    #[allow(dead_code)]
    order: Arc<Mutex<VecDeque<String>>>,
    /// Paths whose thumbnail has not been decoded yet.
    pending: Arc<Mutex<HashSet<String>>>,
    /// The queue the decoder thread reads from.
    sender: Sender<String>,
}

impl ThumbnailCache {
    /// Create a cache.
    pub fn new(ctx: egui::Context) -> Self {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let order = Arc::new(Mutex::new(VecDeque::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let (sender, receiver) = std::sync::mpsc::channel::<String>();

        {
            let entries = entries.clone();
            let order = order.clone();
            let pending = pending.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("thumbnail-decoder".into())
                .spawn(move || {
                    while let Ok(path) = receiver.recv() {
                        let decoded = decode_thumbnail(Path::new(&path)).map(Arc::new);
                        let key = path.to_ascii_lowercase();
                        let mut entries = entries.lock();
                        let mut order = order.lock();
                        entries.insert(key.clone(), decoded);
                        order.push_back(key);
                        // The cache has a **limit**: a thumbnail is a picture
                        // of a few hundred kilobytes, and a long walk through
                        // the folders would otherwise pile them up for ever.
                        while order.len() > THUMBNAIL_LIMIT {
                            if let Some(oldest) = order.pop_front() {
                                entries.remove(&oldest);
                            }
                        }
                        drop(order);
                        drop(entries);
                        pending.lock().remove(&path.to_ascii_lowercase());
                        ctx.request_repaint();
                    }
                })
                .ok();
        }

        Self {
            entries,
            order,
            pending,
            sender,
        }
    }

    /// Get the thumbnail for `path`, scheduling the decoding when needed.
    ///
    /// Returns `None` until the background thread has produced the pixels.
    pub fn get(&self, path: &Path) -> Option<Arc<IconImage>> {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if let Some(hit) = self.entries.lock().get(&key) {
            return hit.clone();
        }

        let mut pending = self.pending.lock();
        if pending.insert(key.clone()) {
            let _ = self.sender.send(path.to_string_lossy().to_string());
        }
        drop(pending);

        // **No repaint is asked for here.** This is called while the window is
        // being painted, and a repaint asked for during a paint is an
        // immediate one: the window would draw the next frame at once, and
        // the next frame would ask for a repaint as well — a loop that runs
        // the graphics card as fast as it can go.
        //
        // The thread that decodes the picture asks for the repaint when it is
        // done, and that is the frame in which the picture appears.
        None
    }
}

/// Decode an image and shrink it to a thumbnail.
fn decode_thumbnail(path: &Path) -> Option<IconImage> {
    let image = image::open(path).ok()?;
    let thumbnail = if image.width() > THUMBNAIL_SIZE || image.height() > THUMBNAIL_SIZE {
        image.thumbnail(THUMBNAIL_SIZE, THUMBNAIL_SIZE)
    } else {
        image
    };
    let rgba = thumbnail.to_rgba8();
    Some(IconImage {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

/// Draw the preview pane.
///
/// `settled` says that the pointer has been resting on the row long enough
/// for it to be looked at properly.
///
/// `read_media` says that the user wants the picture of an image and the
/// player of a video to be shown. Both mean **reading the whole file from
/// the disk**, so they are off unless the user asks for them: browsing the
/// list must not touch the disk.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    content: &PreviewContent,
    palette: &Palette,
    opacity: f32,
    thumbnails: &ThumbnailCache,
    details: &DetailsCache,
    video: Option<&mut VideoPlayer>,
    ctx: &egui::Context,
    settled: bool,
    read_media: bool,
) {
    let rect = ui.available_rect_before_wrap();
    let mut painter = ui.painter().clone();
    painter.set_clip_rect(rect);
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(8),
        palette.surface_alt.gamma_multiply(opacity * 0.7),
    );

    let inner = rect.shrink(12.0);
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(inner);

    match content {
        PreviewContent::Text {
            title,
            body,
            markdown,
        } => {
            if !title.is_empty() {
                child.add_space(2.0);
                child.label(
                    RichText::new(title)
                        .color(palette.text.gamma_multiply(opacity))
                        .strong()
                        .size(14.0),
                );
                child.add_space(6.0);
            }
            if *markdown {
                render_markdown(&mut child, body, palette, opacity);
            } else {
                child.label(
                    RichText::new(body)
                        .color(palette.text_dim.gamma_multiply(opacity))
                        .size(12.5),
                );
            }
        }
        PreviewContent::File {
            path,
            name,
            image,
            video: is_video,
            size,
            modified,
        } => {
            child.add_space(2.0);
            child.label(
                RichText::new(name)
                    .color(palette.text.gamma_multiply(opacity))
                    .strong()
                    .size(14.0),
            );
            child.add_space(2.0);
            child.label(
                RichText::new(path)
                    .color(palette.text_faint.gamma_multiply(opacity))
                    .size(11.0),
            );
            child.add_space(10.0);

            // The picture of an image file.
            //
            // Decoding a picture means reading the whole file from the disk,
            // so it only happens when the user asked for it (the setting) —
            // and then only once the pointer has come to rest on the row.
            if let Some(path) = image {
                if read_media {
                    if settled {
                        if let Some(image) = thumbnails.get(path) {
                            let available = child.available_width();
                            let ratio = image.height as f32 / image.width.max(1) as f32;
                            let size = Vec2::new(
                                available,
                                (available * ratio).min(child.available_height() * 0.45),
                            );
                            let handle = ctx.load_texture(
                                "thumbnail",
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [image.width, image.height],
                                    &image.rgba,
                                ),
                                egui::TextureOptions::LINEAR,
                            );
                            let (rect, _) = child.allocate_exact_size(size, Sense::hover());
                            child.painter().image(
                                handle.id(),
                                rect,
                                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                            child.add_space(10.0);
                        } else {
                            child.label(
                                RichText::new("正在打开图片…")
                                    .color(palette.text_faint.gamma_multiply(opacity))
                                    .size(11.5),
                            );
                            child.add_space(8.0);
                        }
                    }
                }
            }

            // The player of a video file.
            if *is_video && read_media {
                if let Some(player) = video {
                    show_video(&mut child, player, palette, opacity, ctx);
                    child.add_space(10.0);
                }
            }

            // What the index already knows about the file.
            //
            // **Nothing here is read from the disk.** The size and the time
            // come with the result — the index built them when it read the
            // master file table — so showing them is free.
            let mut rows: Vec<(&str, String)> = Vec::new();
            if let Some(size) = size {
                rows.push(("大小", format_size(*size)));
            }
            if let Some(modified) = modified {
                rows.push(("修改时间", format_unix(*modified as i64)));
            }
            for (label, value) in rows {
                detail_row(&mut child, label, &value, palette, opacity);
                child.add_space(2.0);
            }

            // The deeper details — the index number of the file, its streams,
            // what opens it, how many things a folder holds.
            //
            // They need the file to be opened, so they are **only shown if
            // they have already been read**: they are read when the item is
            // opened, not while the list is being browsed. Browsing the list
            // must not touch the disk at all.
            let details = details.get_cached(&PathBuf::from(path));
            if let Some(details) = details {
                for (title, rows) in details.groups() {
                    if rows.is_empty() {
                        continue;
                    }
                    child.add_space(6.0);
                    child.label(
                        RichText::new(title)
                            .color(palette.text_faint.gamma_multiply(opacity))
                            .size(10.5)
                            .strong(),
                    );
                    child.add_space(2.0);
                    for (label, value) in rows {
                        detail_row(&mut child, label, &value, palette, opacity);
                    }
                }
            }
        }
        PreviewContent::Image { path, caption } => {
            let file_path = PathBuf::from(path);
            // **The picture is only read when the user asked for it.**
            // Decoding it means reading the whole file from the disk, and the
            // setting that says "do not read media" has to be obeyed here
            // too — it used to be forgotten on this branch, and selecting a
            // picture read it off the disk even with the setting off.
            let picture = if read_media {
                thumbnails.get(&file_path)
            } else {
                None
            };
            if let Some(image) = picture {
                let available = child.available_size();
                let ratio = image.height as f32 / image.width.max(1) as f32;
                let width = available.x.min(available.y / ratio.max(0.01));
                let size = Vec2::new(width, width * ratio);
                let handle = ctx.load_texture(
                    "preview",
                    egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba),
                    egui::TextureOptions::LINEAR,
                );
                let (rect, _) = child.allocate_exact_size(size, Sense::hover());
                child.painter().image(
                    handle.id(),
                    rect,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else if read_media {
                child.label(
                    RichText::new("载入中…")
                        .color(palette.text_faint.gamma_multiply(opacity))
                        .size(12.0),
                );
            }
            if !caption.is_empty() {
                child.add_space(6.0);
                child.label(
                    RichText::new(caption)
                        .color(palette.text_faint.gamma_multiply(opacity))
                        .size(11.0),
                );
            }
        }
    }
}

/// One row of the details: the name of the thing and its value.
fn detail_row(ui: &mut Ui, label: &str, value: &str, palette: &Palette, opacity: f32) {
    ui.horizontal_top(|ui| {
        ui.add_sized(
            [62.0, 0.0],
            egui::Label::new(
                RichText::new(label)
                    .color(palette.text_faint.gamma_multiply(opacity))
                    .size(11.0),
            ),
        );
        ui.label(
            RichText::new(value)
                .color(palette.text_dim.gamma_multiply(opacity))
                .size(11.5),
        );
    });
}

/// The player of a video file.
fn show_video(
    ui: &mut Ui,
    player: &mut VideoPlayer,
    palette: &Palette,
    opacity: f32,
    ctx: &egui::Context,
) {
    // The picture.
    let available = ui.available_width();
    let height = (available * 0.62).min(ui.available_height() * 0.45);
    match player.frame() {
        Some(frame) => {
            let ratio = frame.height as f32 / frame.width.max(1) as f32;
            let width = (height / ratio.max(0.01)).min(available);
            let size = Vec2::new(width, width * ratio);
            let handle = ctx.load_texture(
                "video-frame",
                egui::ColorImage::from_rgba_unmultiplied(
                    [frame.width, frame.height],
                    &frame.rgba,
                ),
                egui::TextureOptions::LINEAR,
            );
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().image(
                handle.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None => {
            let text = match player.error() {
                Some(error) => error,
                None => "正在打开视频…".to_owned(),
            };
            ui.allocate_exact_size(Vec2::new(available, height), Sense::hover());
            ui.label(
                RichText::new(text)
                    .color(palette.text_faint.gamma_multiply(opacity))
                    .size(11.5),
            );
        }
    }

    // The controls.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let playing = player.is_playing();
        let text = if playing { "暂停" } else { "播放" };
        if ui
            .button(
                RichText::new(text)
                    .color(palette.text.gamma_multiply(opacity))
                    .size(11.5),
            )
            .clicked()
        {
            if playing {
                player.pause();
            } else {
                player.play();
            }
        }
        let position = player.position();
        let duration = player.duration();
        let mut label = format_millis(position);
        if let Some(duration) = duration {
            label.push_str(&format!(" / {}", format_millis(duration)));
        }
        ui.label(
            RichText::new(label)
                .color(palette.text_faint.gamma_multiply(opacity))
                .size(11.0),
        );
    });
}

/// A number of milliseconds as `1:05`.
pub fn format_millis(millis: u64) -> String {
    let total = millis / 1000;
    format!("{}:{:02}", total / 60, total % 60)
}

/// Render a markdown document.
fn render_markdown(ui: &mut Ui, body: &str, palette: &Palette, opacity: f32) {
    let blocks = markdown::parse(body);
    for block in blocks {
        render_block(ui, &block, palette, opacity);
    }
}

fn render_block(ui: &mut Ui, block: &Block, palette: &Palette, opacity: f32) {
    match block {
        Block::Heading { level, spans } => {
            ui.add_space(4.0);
            let size = match level {
                1 => 16.0,
                2 => 14.5,
                3 => 13.5,
                _ => 12.5,
            };
            ui.label(spans_to_rich(spans, palette, opacity, size, true));
        }
        Block::Paragraph(spans) => {
            ui.label(spans_to_rich(spans, palette, opacity, 12.5, false));
            ui.add_space(4.0);
        }
        Block::ListItem { ordered, spans } => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if *ordered { "1." } else { "•" })
                        .color(palette.text_faint.gamma_multiply(opacity))
                        .size(12.5),
                );
                ui.label(spans_to_rich(spans, palette, opacity, 12.5, false));
            });
        }
        Block::Code(code) => {
            ui.add_space(2.0);
            let frame = egui::Frame::NONE
                .fill(palette.surface.gamma_multiply(opacity))
                .corner_radius(4)
                .inner_margin(egui::Margin::same(6));
            frame.show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(code)
                            .color(palette.text_dim.gamma_multiply(opacity))
                            .monospace()
                            .size(11.5),
                    )
                    .wrap(),
                );
            });
            ui.add_space(4.0);
        }
        Block::Quote(spans) => {
            ui.horizontal(|ui| {
                let rect = ui.available_rect_before_wrap();
                ui.painter().rect_filled(
                    Rect::from_min_size(
                        egui::pos2(rect.left(), rect.top()),
                        egui::Vec2::new(2.0, rect.height()),
                    ),
                    egui::CornerRadius::same(1),
                    palette.accent.gamma_multiply(opacity * 0.6),
                );
                ui.add_space(6.0);
                ui.label(spans_to_rich(spans, palette, opacity, 12.5, false));
            });
        }
        Block::Rule => {
            ui.add_space(4.0);
            let rect = ui.available_rect_before_wrap();
            ui.painter().hline(
                rect.x_range(),
                rect.center().y,
                Stroke::new(1.0, palette.border.gamma_multiply(opacity)),
            );
            ui.add_space(4.0);
        }
    }
}

/// Turn styled spans into a layout job.
///
/// `egui` renders a label from a single text format, so a document with
/// mixed styles is assembled as a job of sections — one per span.
fn spans_to_rich(
    spans: &[Span],
    palette: &Palette,
    opacity: f32,
    size: f32,
    strong: bool,
) -> egui::text::LayoutJob {
    let base = palette.text_dim.gamma_multiply(opacity);
    let mut job = egui::text::LayoutJob::default();

    for span in spans {
        let color = match span.style {
            SpanStyle::Link => palette.accent.gamma_multiply(opacity),
            _ if strong => palette.text.gamma_multiply(opacity),
            _ => base,
        };
        let mut format = egui::text::TextFormat {
            font_id: FontId::proportional(size),
            color,
            ..Default::default()
        };
        match span.style {
            SpanStyle::Code => format.font_id = FontId::monospace(size),
            SpanStyle::Italic => format.italics = true,
            SpanStyle::Link => format.underline = Stroke::new(1.0, color),
            _ => {}
        }
        job.append(&span.text, 0.0, format);
    }

    job
}

/// The font the pane uses for the body text.
pub fn body_font() -> FontId {
    FontId::proportional(12.5)
}

/// The text style the body text uses.
pub fn body_text_style() -> TextStyle {
    TextStyle::Body
}

/// A faint border colour, used by the tests.
pub fn border_color(palette: &Palette) -> Color32 {
    palette.border
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_the_way_a_file_manager_does() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_plugin_description_becomes_text() {
        let result = LauncherResult {
            title: "Answer".into(),
            preview: Some(PreviewData {
                description: "**bold**".into(),
                content_type: PreviewContentType::Markdown,
                ..Default::default()
            }),
            ..Default::default()
        };
        match derive(&result).expect("content") {
            PreviewContent::Text { body, markdown, .. } => {
                assert_eq!(body, "**bold**");
                assert!(markdown);
            }
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn a_file_result_becomes_file_details() {
        let result = LauncherResult {
            title: "notes.txt".into(),
            plugin_id: "file".into(),
            copy_text: r"C:\Users\test\notes.txt".into(),
            ..Default::default()
        };
        match derive(&result).expect("content") {
            PreviewContent::File { name, image, video, .. } => {
                assert_eq!(name, "notes.txt");
                assert!(image.is_none());
                assert!(!video);
            }
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn a_result_without_a_preview_has_none() {
        let result = LauncherResult {
            title: "Chrome".into(),
            plugin_id: "program".into(),
            ..Default::default()
        };
        assert!(derive(&result).is_none());
    }

    #[test]
    fn an_image_file_gets_a_picture() {
        let result = LauncherResult {
            title: "photo.png".into(),
            plugin_id: "file".into(),
            copy_text: r"C:\Users\test\photo.png".into(),
            ..Default::default()
        };
        match derive(&result).expect("content") {
            PreviewContent::File { image, .. } => assert!(image.is_some()),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn a_video_file_gets_a_player() {
        let result = LauncherResult {
            title: "clip.mp4".into(),
            plugin_id: "file".into(),
            copy_text: r"C:\Users\test\clip.mp4".into(),
            ..Default::default()
        };
        match derive(&result).expect("content") {
            PreviewContent::File { video, .. } => assert!(video),
            other => panic!("unexpected content: {other:?}"),
        }
    }

    #[test]
    fn formats_a_timestamp() {
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(format_time(Some(time)), "2023-11-14 22:13");
        assert_eq!(format_time(None), "");
    }
}
