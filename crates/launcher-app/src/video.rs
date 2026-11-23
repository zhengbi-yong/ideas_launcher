//! 视频播放。
//!
//! 播的是 **FFmpeg** —— 它是这个世界上最齐全的解码器:H.264、HEVC、VP9、AV1、
//! ProRes 都有,硬件解码也有。而 Windows 自己那套 Media Foundation 在很多机器上
//! 是解不了 H.264 的:本机注册了十七个视频解码器,却一个 H.264 的都没有
//! (问系统「谁会读 H.264」,答案是空的),所以那一路走不通。
//!
//! FFmpeg 是以**子进程**的方式被叫起来的:它把画面按原速写进标准输出,
//! 这一边按帧读出来,画到预览面板上。这样做的好处是**不用改构建**:
//! 启动器仍然是零依赖的 Rust 程序,FFmpeg 只是跟着安装包一起来的一组
//! 可执行文件(装在 `runtime\ffmpeg` 目录里)。
//!
//! 硬件解码走的是 `-hwaccel d3d11va`:NVIDIA、AMD、Intel 的显卡都认。
//! 万一驱动不认,程序会自己退回软件解码 —— 慢一点,但是照样能看。
//!
//! 没有音频:预览面板里放一段会自己出声的音频,比放一段无声的更糟。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command as Process, Stdio};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as PlMutex;

/// The widest the frames are asked in.
///
/// The pane is two hundred and sixty eight pixels wide; asking for more means
/// moving more pixels than anybody sees.
const FRAME_WIDTH: u32 = 480;

/// A video bigger than this is not started on its own when the **processor**
/// has to decode it: four thousand pixels sixty times a second costs half a
/// core, and nobody asked for that. When the graphics card does the work,
/// the video starts on its own however big it is.
const AUTOPLAY_WIDTH: u32 = 1920;
const AUTOPLAY_HEIGHT: u32 = 1080;

/// One frame of the video.
pub struct Frame {
    /// The width of the picture.
    pub width: usize,
    /// The height of the picture.
    pub height: usize,
    /// The pixels, four bytes each, in the order red, green, blue, alpha.
    pub rgba: Vec<u8>,
}

/// What is known about a video file.
#[derive(Debug, Clone, Default)]
pub struct VideoInfo {
    /// How long the video runs, in milliseconds.
    pub duration: Option<u64>,
    /// The width of the picture.
    pub width: u32,
    /// The height of the picture.
    pub height: u32,
    /// How many pictures there are in a second.
    pub frame_rate: Option<f64>,
    /// The name of the codec.
    pub codec: Option<String>,
}

/// Whether the extension belongs to a video.
pub fn is_video_extension(extension: &str) -> bool {
    let extension = extension.trim().trim_start_matches('.').to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "flv" | "mpg" | "mpeg" | "ts"
            | "m2ts" | "3gp" | "rmvb" | "vob" | "ogv" | "mts" | "asf" | "rm" | "divx"
    )
}

/// What is already known about the videos that were looked at.
///
/// The pointer chooses the row it is over, and the same videos are passed
/// over again and again. The answer is kept, so that FFmpeg is asked once
/// per file and not once per visit.
static PROBED: std::sync::LazyLock<
    PlMutex<std::collections::HashMap<PathBuf, Option<VideoInfo>>>,
> = std::sync::LazyLock::new(|| PlMutex::new(std::collections::HashMap::new()));

/// How many videos are remembered.
const PROBED_LIMIT: usize = 256;

/// Ask the file about itself.
///
/// It is cheap enough to be done for every file that is looked at: one
/// `ffprobe` process, a few milliseconds.
pub fn probe(path: &Path) -> Option<VideoInfo> {
    let key = path.to_path_buf();
    if let Some(hit) = PROBED.lock().get(&key) {
        return hit.clone();
    }
    let info = probe_now(path);
    let mut cache = PROBED.lock();
    if cache.len() > PROBED_LIMIT {
        cache.clear();
    }
    cache.insert(key, info.clone());
    info
}

/// Ask FFmpeg about a file.
fn probe_now(path: &Path) -> Option<VideoInfo> {
    let dir = ffmpeg_dir()?;
    let mut command = Process::new(dir.join("ffprobe.exe"));
    hide_console(&mut command);
    let output = command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,avg_frame_rate,codec_name:format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let stream = json.get("streams")?.get(0)?;
    let width = stream.get("width")?.as_u64()? as u32;
    let height = stream.get("height")?.as_u64()? as u32;
    Some(VideoInfo {
        duration: json
            .get("format")
            .and_then(|format| format.get("duration"))
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<f64>().ok())
            .map(|value| (value * 1000.0) as u64),
        width,
        height,
        frame_rate: stream
            .get("avg_frame_rate")
            .and_then(|value| value.as_str())
            .and_then(parse_rate),
        codec: stream
            .get("codec_name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    })
}

/// Read a frame rate: "25/1" or "12.518417".
fn parse_rate(text: &str) -> Option<f64> {
    let (numerator, denominator) = match text.split_once('/') {
        Some((numerator, denominator)) => {
            let numerator = numerator.trim().parse::<f64>().ok()?;
            let denominator = denominator.trim().parse::<f64>().ok()?;
            (numerator, denominator)
        }
        None => {
            let value = text.trim().parse::<f64>().ok()?;
            (value, 1.0)
        }
    };
    if denominator <= 0.0 {
        None
    } else {
        Some(numerator / denominator)
    }
}

/// Where the FFmpeg programs are.
///
/// They come with the installation: the installer puts them in
/// `runtime\ffmpeg` next to the program. A copy that is on the path also
/// works, and so does the environment variable `IDEAS_FFMPEG` — that one is
/// for the development build, where the files are not installed yet.
pub fn ffmpeg_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("IDEAS_FFMPEG") {
        candidates.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("runtime").join("ffmpeg"));
            candidates.push(dir.join("ffmpeg"));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            if dir.is_empty() {
                continue;
            }
            candidates.push(PathBuf::from(dir));
        }
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("ffmpeg.exe").exists())
}

/// The `CREATE_NO_WINDOW` flag, so that a console program does not flash a
/// window while it starts.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Keep a console program from flashing a window while it starts.
///
/// FFmpeg and ffprobe are console programs. Started from a windowed program
/// without this flag, they open a console window of their own — and that
/// window **takes the focus away from the launcher**, which then hides
/// itself and the list of results vanishes with it.
#[cfg(windows)]
fn hide_console(command: &mut Process) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

/// The same on the other platforms: there is nothing to do there.
#[cfg(not(windows))]
fn hide_console(_command: &mut Process) {}

/// What the player is told to do.
enum Command {
    Play,
    Pause,
    Seek(u64),
}

/// The state the pane draws.
#[derive(Default)]
struct PlayerState {
    /// The frame that is on the screen.
    frame: Option<Arc<Frame>>,
    /// Where the video is, in milliseconds.
    position: u64,
    /// How long the video runs, in milliseconds.
    duration: Option<u64>,
    /// Whether the video is playing.
    playing: bool,
    /// What went wrong, when something did.
    error: Option<String>,
}

/// A video that plays in the preview pane.
pub struct VideoPlayer {
    shared: Arc<PlMutex<PlayerState>>,
    commands: Sender<Command>,
    ctx: egui::Context,
    /// The file that is playing.
    path: String,
}

impl VideoPlayer {
    /// Start a player for a file.
    ///
    /// `known` is what the details pane already found out about the file. It
    /// saves a second `ffprobe` — the pointer chooses the row it is over, and
    /// every row it passes would otherwise start two programs.
    pub fn new(path: &Path, ctx: egui::Context, known: Option<VideoInfo>) -> Self {
        let shared = Arc::new(PlMutex::new(PlayerState::default()));
        let (sender, receiver): (Sender<Command>, Receiver<Command>) = std::sync::mpsc::channel();
        let worker = shared.clone();
        let worker_ctx = ctx.clone();
        let file = path.to_path_buf();

        std::thread::Builder::new()
            .name("video-player".into())
            .spawn(move || {
                run_player(file, known, worker, receiver, worker_ctx);
            })
            .ok();

        Self {
            shared,
            commands: sender,
            ctx,
            path: path.to_string_lossy().to_string(),
        }
    }

    /// The file that is playing.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The frame that is on the screen.
    pub fn frame(&self) -> Option<Arc<Frame>> {
        self.shared.lock().frame.clone()
    }

    /// Where the video is, in milliseconds.
    pub fn position(&self) -> u64 {
        self.shared.lock().position
    }

    /// How long the video runs, in milliseconds.
    pub fn duration(&self) -> Option<u64> {
        self.shared.lock().duration
    }

    /// Whether the video is playing.
    pub fn is_playing(&self) -> bool {
        self.shared.lock().playing
    }

    /// What went wrong, when something did.
    pub fn error(&self) -> Option<String> {
        self.shared.lock().error.clone()
    }

    /// Start playing.
    pub fn play(&self) {
        let _ = self.commands.send(Command::Play);
        self.ctx.request_repaint();
    }

    /// Stop playing.
    pub fn pause(&self) {
        let _ = self.commands.send(Command::Pause);
        self.ctx.request_repaint();
    }

    /// Move to a place in the video.
    pub fn seek(&self, millis: u64) {
        let _ = self.commands.send(Command::Seek(millis));
        self.ctx.request_repaint();
    }
}

/// The thread that plays the file.
fn run_player(
    path: PathBuf,
    known: Option<VideoInfo>,
    shared: Arc<PlMutex<PlayerState>>,
    commands: Receiver<Command>,
    ctx: egui::Context,
) {
    let Some(dir) = ffmpeg_dir() else {
        shared.lock().error = Some("没有找到 FFmpeg,视频放不了".to_owned());
        ctx.request_repaint();
        return;
    };
    let info = match known {
        Some(info) => info,
        None => match probe(&path) {
            Some(info) => info,
            None => {
                shared.lock().error = Some("这个视频打不开,FFmpeg 读不了它".to_owned());
                ctx.request_repaint();
                return;
            }
        },
    };
    log::info!(
        "the video {} is opened: {}x{}, {:?}, {:?}",
        path.display(),
        info.width,
        info.height,
        info.codec,
        info.duration
    );

    // The picture is scaled down to what the pane can show: the width is
    // fixed and the height follows the shape of the video.
    let width = info.width.min(FRAME_WIDTH).max(2);
    let height = (((width as u64 * info.height as u64) / info.width.max(1) as u64) as u32) & !1;

    // How long one picture is shown.
    let frame_time = info
        .frame_rate
        .filter(|rate| *rate > 0.0)
        .map(|rate| (1000.0 / rate) as u64)
        .unwrap_or(40);

    // The decoder of the graphics card is asked for first. When the driver
    // does not know it, FFmpeg stops at once — and then the software
    // decoder is used: slower, but the video is seen.
    let mut hardware = true;
    let mut decoder = match Decoder::start(&dir, &path, width, height, 0, true) {
        Some(decoder) => decoder,
        None => {
            shared.lock().error = Some("FFmpeg 起不来,视频放不了".to_owned());
            ctx.request_repaint();
            return;
        }
    };
    let mut poster = decoder.next_frame();
    if poster.is_none() {
        // The driver does not know the hardware decoder: it stopped before
        // it gave a single picture. Start over with the software one.
        hardware = false;
        match Decoder::start(&dir, &path, width, height, 0, false) {
            Some(next) => {
                decoder = next;
                poster = decoder.next_frame();
            }
            None => {
                shared.lock().error = Some("FFmpeg 起不来,视频放不了".to_owned());
                ctx.request_repaint();
                return;
            }
        }
    }
    let Some(poster) = poster else {
        let reason = decoder.take_error().unwrap_or_else(|| "视频放不了".to_owned());
        shared.lock().error = Some(reason);
        ctx.request_repaint();
        return;
    };
    log::info!(
        "the first picture of {} is there; the {} decoder is used",
        path.display(),
        if hardware { "hardware" } else { "software" }
    );

    // The first picture is the cover of the video: it is on the screen even
    // when the video does not start on its own.
    //
    // Whether it starts on its own is decided by who decodes it: the
    // graphics card can do a four thousand pixel video without trouble,
    // the processor cannot.
    let starts_alone =
        hardware || (info.width <= AUTOPLAY_WIDTH && info.height <= AUTOPLAY_HEIGHT);
    {
        let mut state = shared.lock();
        state.duration = info.duration;
        state.frame = Some(Arc::new(poster));
        state.position = 0;
        state.playing = starts_alone;
    }
    ctx.request_repaint();

    // A video that waits for the button does not need a decoder running.
    let mut decoder = if starts_alone { Some(decoder) } else { None };

    let mut position = 0u64;
    let mut wanted_seek: Option<u64> = None;
    // Whether the video ran to its end and waits to be started again.
    let mut at_end = false;

    loop {
        // The commands of the pane.
        loop {
            match commands.try_recv() {
                Ok(Command::Play) => {
                    shared.lock().playing = true;
                }
                Ok(Command::Pause) => {
                    shared.lock().playing = false;
                }
                Ok(Command::Seek(millis)) => {
                    wanted_seek = Some(millis);
                    shared.lock().playing = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Some(millis) = wanted_seek.take() {
            // A jump means starting over from that place: FFmpeg is quick
            // about it, and the pictures keep coming in the same order.
            match Decoder::start(&dir, &path, width, height, millis, hardware) {
                Some(next) => {
                    decoder = Some(next);
                    position = millis;
                    at_end = false;
                }
                None => {
                    shared.lock().error = Some("跳不过去".to_owned());
                    ctx.request_repaint();
                    return;
                }
            }
        }

        if !shared.lock().playing {
            // Paused: wait for the next command without burning the
            // processor, and let the decoder go — a decoder that is not
            // needed holds on to the file and to the graphics card.
            decoder = None;
            match commands.recv_timeout(Duration::from_millis(60)) {
                Ok(Command::Play) => shared.lock().playing = true,
                Ok(Command::Pause) => {}
                Ok(Command::Seek(millis)) => {
                    wanted_seek = Some(millis);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }

        if decoder.is_none() {
            // The video starts: from where it was left, or from the
            // beginning when it had already run to its end.
            let start = if at_end { 0 } else { position };
            match Decoder::start(&dir, &path, width, height, start, hardware) {
                Some(next) => {
                    decoder = Some(next);
                    position = start;
                    at_end = false;
                }
                None => {
                    shared.lock().error = Some("FFmpeg 起不来,视频放不了".to_owned());
                    ctx.request_repaint();
                    return;
                }
            }
        }

        let picture = decoder.as_mut().and_then(|decoder| decoder.next_frame());
        match picture {
            Some(frame) => {
                position = position.saturating_add(frame_time);
                {
                    let mut state = shared.lock();
                    state.frame = Some(Arc::new(frame));
                    state.position = position;
                }
                ctx.request_repaint();
            }
            None => {
                // The file is over, or FFmpeg gave up. The last picture
                // stays on the screen, and the video can be run again.
                let reason = decoder.as_mut().and_then(|decoder| decoder.take_error());
                decoder = None;
                at_end = true;
                {
                    let mut state = shared.lock();
                    state.playing = false;
                    if let Some(reason) = reason {
                        state.error = Some(reason);
                    }
                }
                ctx.request_repaint();
            }
        }
    }
}

/// What FFmpeg said on its error stream, kept by another thread so that the
/// pipe is never full and never blocks the pictures.
type Saying = Arc<PlMutex<String>>;

/// The FFmpeg process that hands out the frames.
struct Decoder {
    /// The process itself.
    child: Child,
    /// The pictures it writes.
    stdout: Option<ChildStdout>,
    /// The width of the pictures.
    width: u32,
    /// The height of the pictures.
    height: u32,
    /// How many bytes one picture takes.
    size: usize,
    /// What FFmpeg said when it stopped.
    saying: Saying,
}

impl Decoder {
    /// Start FFmpeg on a file.
    ///
    /// With `hardware` the decoder of the graphics card is asked for first.
    fn start(
        dir: &Path,
        path: &Path,
        width: u32,
        height: u32,
        seek: u64,
        hardware: bool,
    ) -> Option<Self> {
        let mut child = spawn(dir, path, width, height, seek, hardware)?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take();
        let saying: Saying = Arc::new(PlMutex::new(String::new()));
        if let Some(mut stderr) = stderr {
            // Another thread reads what FFmpeg says, so that the error
            // stream can never block the pictures.
            let sink = saying.clone();
            std::thread::Builder::new()
                .name("video-ffmpeg-says".into())
                .spawn(move || {
                    let mut buffer = [0u8; 1024];
                    loop {
                        match stderr.read(&mut buffer) {
                            Ok(0) => break,
                            Ok(count) => {
                                let text = String::from_utf8_lossy(&buffer[..count]);
                                for part in text.split(['\r', '\n']) {
                                    let part = part.trim();
                                    if part.is_empty() {
                                        continue;
                                    }
                                    *sink.lock() = part.to_owned();
                                }
                            }
                            Err(_) => break,
                        }
                    }
                })
                .ok();
        }
        let size = (width as usize) * (height as usize) * 4;
        Some(Self {
            child,
            stdout: Some(stdout),
            width,
            height,
            size,
            saying,
        })
    }

    /// The next picture.
    fn next_frame(&mut self) -> Option<Frame> {
        let stdout = self.stdout.as_mut()?;
        let mut buffer = vec![0u8; self.size];
        match stdout.read_exact(&mut buffer) {
            Ok(()) => Some(Frame {
                width: self.width as usize,
                height: self.height as usize,
                rgba: buffer,
            }),
            Err(_) => {
                // The file is over, or FFmpeg stopped with an error.
                None
            }
        }
    }

    /// What FFmpeg said when it stopped — nothing at all when it simply
    /// reached the end of the file.
    fn take_error(&mut self) -> Option<String> {
        let text = self.saying.lock().trim().to_owned();
        if text.is_empty() {
            None
        } else {
            Some(text)
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // FFmpeg is a child process: it does not stop by itself when nobody
        // looks at it any more.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start the FFmpeg process.
fn spawn(
    dir: &Path,
    path: &Path,
    width: u32,
    height: u32,
    seek: u64,
    hardware: bool,
) -> Option<Child> {
    let mut command = Process::new(dir.join("ffmpeg.exe"));
    hide_console(&mut command);
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]);
    if hardware {
        // The decoder of the graphics card. When the driver does not know it,
        // FFmpeg says so on its error stream and stops.
        command.args(["-hwaccel", "d3d11va"]);
    }
    if seek > 0 {
        command.args(["-ss", &format!("{:.3}", seek as f64 / 1000.0)]);
    }
    // Read the file at the speed it plays, so that the pictures come out at
    // the right pace.
    command.arg("-re");
    command.arg("-i").arg(path);
    command.args([
        "-an",
        "-vf",
        &format!("scale={width}:{height}"),
        "-pix_fmt",
        "rgba",
        "-f",
        "rawvideo",
        "-",
    ]);
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_rate_is_read() {
        assert_eq!(parse_rate("25/1"), Some(25.0));
        assert_eq!(parse_rate("30000/1001"), Some(30000.0 / 1001.0));
        assert_eq!(parse_rate("12.518417"), Some(12.518417));
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(parse_rate("abc"), None);
    }

    #[test]
    fn the_video_extensions_are_known() {
        assert!(is_video_extension("mp4"));
        assert!(is_video_extension("MKV"));
        assert!(!is_video_extension("txt"));
    }
}
