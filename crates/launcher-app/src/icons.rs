//! Lazy icon loading.
//!
//! Flow Launcher preloads every icon at startup — the log shows
//! `Preload images cost <10209ms>` at the median. That single decision is
//! the largest reason its cold start takes seconds.
//!
//! Here an icon is only touched when its row is actually drawn, and the
//! decoded RGBA bytes are kept in a small LRU cache. The first paint of a
//! row costs one icon extraction; every later paint is free.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use parking_lot::Mutex;
use std::sync::mpsc::Sender;

use crate::winapi::*;

/// Turn a decoded icon into the picture the graphics card draws.
pub fn to_color_image(image: &IconImage) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([image.width, image.height], &image.rgba)
}

/// A decoded icon.
#[derive(Clone)]
pub struct IconImage {
    pub width: usize,
    pub height: usize,
    /// RGBA, 8 bits per channel.
    pub rgba: Vec<u8>,
}

/// A bounded cache of decoded icons.
///
/// Decoding an icon reads the resources of an executable, which takes a few
/// milliseconds for a large program. The work therefore happens on a
/// background thread: the row asks for the icon, gets a placeholder right
/// away, and the real pixels show up a frame later.
///
/// This is the same split Flow Launcher makes with its `ImageLoader` — the
/// difference is that ours never blocks the window at all.
pub struct IconCache {
    entries: Arc<Mutex<HashMap<String, Option<std::sync::Arc<IconImage>>>>>,
    /// The icons that were read from the disk and written down.
    ///
    /// These are **never thrown away**. The other entries are a cache and may
    /// be dropped when it is full — but an icon that was read once and put in
    /// our own data must not be read a second time, and must not be written a
    /// second time either. That is the whole point of the store.
    learned: Arc<Mutex<HashMap<String, Option<std::sync::Arc<IconImage>>>>>,
    /// The textures of the icons, by path.
    ///
    /// The pixels are decoded once, but the texture was put on the graphics
    /// card again for every row and every frame: `load_texture` with a name
    /// that is already there replaces the texture, so every visible icon was
    /// uploaded again sixty times a second — and the pixels were copied into
    /// a new buffer each time. The handle is kept here instead, and the
    /// texture is made once.
    textures: Arc<Mutex<HashMap<String, egui::TextureHandle>>>,
    /// The icon of the program that opens a kind of file, by extension.
    ///
    /// Every text file is opened by the same program, so the icon is the same
    /// too — and it is looked up once per kind of file instead of once per
    /// file.
    associated: Arc<Mutex<HashMap<String, Option<std::sync::Arc<IconImage>>>>>,
    /// The paths that are known to be folders.
    ///
    /// A folder has an icon of its own, and it is not the icon of a kind of
    /// file — so the folders are remembered apart from the extensions.
    folders: Arc<Mutex<HashSet<String>>>,
    /// Paths whose icon has not been decoded yet.
    pending: Arc<Mutex<HashSet<String>>>,
    /// The queue the decoder thread reads from.
    sender: std::sync::mpsc::Sender<String>,
}

/// Where the icons we have learned are kept.
///
/// The point of this store is that **looking through a list of results must
/// not read the disk**. The icon of a program is read the first time the
/// program is actually opened — the file is being opened anyway, so that is
/// the moment to take its icon — and it is kept here for the next time.
pub struct IconStore {
    folder: PathBuf,
}

impl IconStore {
    /// The store in the launcher's own data directory.
    pub fn new() -> Self {
        Self::in_folder(crate::paths::data_directory().join("Icons"))
    }

    /// A store in a given folder.
    ///
    /// The tests use it to keep to themselves: a test that ran against the
    /// real store would see the icons the user has learned, and would fail
    /// on a machine that has any.
    pub fn in_folder(folder: PathBuf) -> Self {
        Self { folder }
    }

    /// The file that holds an icon, by its key.
    ///
    /// The name is a hash of the key, and the key itself is written into the
    /// file — so the store can be read back without an index of its own.
    fn file_for_key(&self, key: &str) -> PathBuf {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in key.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.folder.join(format!("{hash:016x}.icon"))
    }

    /// Every icon we have learned, with the path it belongs to.
    ///
    /// This is read once, when the launcher starts — not while the user is
    /// looking at the results.
    pub fn load(&self) -> Vec<(String, std::sync::Arc<IconImage>)> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.folder) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Some((key, image)) = decode_stored(&bytes) {
                out.push((key, std::sync::Arc::new(image)));
            }
        }
        out
    }

    /// Remember the icon of a path.
    ///
    /// It is written the first time the program is opened, and never again:
    /// a file that is already in the store is left alone, so the disk is not
    /// written for nothing.
    pub fn save(&self, path: &Path, image: &IconImage) {
        let key = path.to_string_lossy().to_ascii_lowercase();
        self.save_key(&key, image);
    }

    /// Remember an icon under a key.
    ///
    /// The key is either a path or the name of a kind of file (`ext:txt`).
    pub fn save_key(&self, key: &str, image: &IconImage) {
        if std::fs::create_dir_all(&self.folder).is_err() {
            return;
        }
        let file = self.file_for_key(key);
        if file.exists() {
            // We already have it — writing it again would only wear the disk
            // out for nothing.
            return;
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(&(image.width as u32).to_le_bytes());
        bytes.extend_from_slice(&(image.height as u32).to_le_bytes());
        bytes.extend_from_slice(&image.rgba);
        let _ = std::fs::write(file, bytes);
    }
}

/// The icon of a path as it is kept on the disk.
fn decode_stored(bytes: &[u8]) -> Option<(String, IconImage)> {
    if bytes.len() < 8 {
        return None;
    }
    let length = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    let key = String::from_utf8(bytes[4..4 + length].to_vec()).ok()?;
    let width = u32::from_le_bytes(bytes[4 + length..8 + length].try_into().ok()?) as usize;
    let height = u32::from_le_bytes(bytes[8 + length..12 + length].try_into().ok()?) as usize;
    let start = 12 + length;
    let pixels = bytes.len().checked_sub(start)?;
    if pixels != width * height * 4 {
        return None;
    }
    Some((
        key,
        IconImage {
            width,
            height,
            rgba: bytes[start..].to_vec(),
        },
    ))
}

impl IconCache {
    /// Create a cache holding at most `capacity` decoded icons.
    ///
    /// The icons it has learned are kept in the launcher's own data
    /// directory.
    pub fn new(capacity: usize, ctx: egui::Context) -> Self {
        Self::with_store(capacity, ctx, std::sync::Arc::new(IconStore::new()))
    }

    /// Create a cache that keeps the icons it has learned in a given store.
    pub fn with_store(
        capacity: usize,
        ctx: egui::Context,
        store: std::sync::Arc<IconStore>,
    ) -> Self {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let learned = Arc::new(Mutex::new(HashMap::new()));
        let textures = Arc::new(Mutex::new(HashMap::<String, egui::TextureHandle>::new()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let associated = Arc::new(Mutex::new(HashMap::new()));
        let folders = Arc::new(Mutex::new(HashSet::new()));
        let (sender, receiver) = std::sync::mpsc::channel::<String>();

        // The icons we have learned are put in before anything else, so that
        // the rows find them without touching the disk.
        //
        // There are two kinds: the icon of a path, and the icon of a *kind*
        // of file — every text file is opened by the same program, so they
        // all share one icon. The second kind is what makes a whole list of
        // documents show their icons after a single read.
        for (key, image) in store.load() {
            if let Some(extension) = key.strip_prefix("ext:") {
                associated.lock().insert(extension.to_string(), Some(image));
                continue;
            }
            entries.lock().insert(key.clone(), Some(image.clone()));
            learned.lock().insert(key.clone(), Some(image));
            order.lock().push(key);
        }

        {
            let entries = entries.clone();
            let learned = learned.clone();
            let order = order.clone();
            let pending = pending.clone();
            let associated = associated.clone();
            let folders = folders.clone();
            let textures = textures.clone();
            let store = store.clone();
            let capacity = capacity;
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("icon-decoder".into())
                .spawn(move || {
                    while let Ok(path) = receiver.recv() {
                        let decoded = {
                            let _timer = launcher_core::profile::phase("icon.decode");
                            decode_icon(std::path::Path::new(&path), &associated)
                        };
                        launcher_core::profile::count("icon.decoded", 1);
                        let key = path.to_ascii_lowercase();
                        if decoded.folder {
                            // A folder is not a kind of file: it must not be
                            // answered with the icon of its extension.
                            folders.lock().insert(key.clone());
                        }
                        if decoded.image.is_none() {
                            log::info!("这个文件没有图标: {path}");
                        }
                        // The icon is kept, so that the next time this
                        // program comes up, its row has an icon without
                        // reading anything.
                        if let Some(image) = decoded.image.as_ref() {
                            store.save(std::path::Path::new(&path), image);
                        }
                        // The icon of a *kind* of file is written down as
                        // well — every file of that kind is opened by the
                        // same program, so they all share one icon. After
                        // this, a whole list of documents shows its icons
                        // without reading anything.
                        if !decoded.folder {
                            if let Some(extension) = extension_key(std::path::Path::new(&path)) {
                                if !carries_own_icon(&extension) {
                                    let shared = associated.lock().get(&extension).cloned().flatten();
                                    if let Some(image) = shared {
                                        store.save_key(&format!("ext:{extension}"), &image);
                                    }
                                }
                            }
                        }
                        // It is now known: it must not be read a second time,
                        // even if the cache is full later on.
                        learned.lock().insert(key.clone(), decoded.image.clone());
                        let mut entries = entries.lock();
                        let mut order = order.lock();
                        entries.insert(key.clone(), decoded.image);
                        order.push(key.clone());

                        // Evict the oldest entries once the cache is full.
                        while order.len() > capacity {
                            if let Some(oldest) = order.first().cloned() {
                                order.remove(0);
                                entries.remove(&oldest);
                                // The texture of an icon that is gone has to
                                // go as well, or the graphics card keeps it
                                // for ever — unless we learned it, in which
                                // case it comes back and we would upload it
                                // again for nothing.
                                if !learned.lock().contains_key(&oldest) {
                                    textures.lock().remove(&oldest);
                                }
                            } else {
                                break;
                            }
                        }
                        drop(order);
                        drop(entries);

                        pending.lock().remove(&key);
                        // Show the icon in the next frame.
                        ctx.request_repaint();
                    }
                })
                .ok();
        }

        Self {
            entries,
            learned,
            textures,
            associated,
            folders,
            pending,
            sender,
        }
    }

    /// The texture of the icon of a path, made once and then kept.
    ///
    /// The texture is what the graphics card draws. Making it again for every
    /// frame means uploading the pixels again for every frame, and the list
    /// is drawn sixty times a second.
    ///
    /// **Nothing is read from the disk here.** Whatever is not in memory is
    /// simply not shown — the row gets its letter instead. The icon is read
    /// the one time the program is opened, and never while the user is
    /// looking at the list.
    pub fn texture(&self, path: &Path, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let image = self.cached(path)?;
        let key = path.to_string_lossy().to_ascii_lowercase();
        if let Some(handle) = self.textures.lock().get(&key) {
            return Some(handle.clone());
        }
        let handle = ctx.load_texture(
            "icon",
            to_color_image(&image),
            egui::TextureOptions::LINEAR,
        );
        self.textures.lock().insert(key, handle.clone());
        Some(handle)
    }

    /// The icon of a path, if it is in memory.
    ///
    /// This is the only way the list gets its icons, and it never touches
    /// the disk: an icon that was not learned yet is simply not shown.
    pub fn cached(&self, path: &Path) -> Option<std::sync::Arc<IconImage>> {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if let Some(hit) = self.lookup(&key) {
            return Some(hit);
        }
        // The icon of the program that opens this kind of file is shared by
        // every file of the kind, so it is often already there. A folder is
        // not a kind of file — it has an icon of its own — and neither is a
        // shortcut or a program.
        if self.folders.lock().contains(&key) {
            return None;
        }
        let extension = extension_key(path)?;
        if carries_own_icon(&extension) {
            return None;
        }
        self.associated.lock().get(&extension).cloned().flatten()
    }

    /// Look a path up in the cache.
    ///
    /// The learned icons are asked for as well: they are kept for good, so
    /// that a program that was opened once never has to be read again.
    fn lookup(&self, key: &str) -> Option<std::sync::Arc<IconImage>> {
        if let Some(hit) = self.entries.lock().get(key) {
            return hit.clone();
        }
        self.learned.lock().get(key).cloned().flatten()
    }

    /// Get the icon for `path`.
    ///
    /// **Nothing is read from the disk here.** The icon is either one we have
    /// already learned — it was learned the first time the program was opened
    /// — or there is none, and the row gets its letter instead.
    ///
    /// The old way read the icon of every row the pointer went over, and the
    /// resources of an executable are not a small thing to read: a dozen rows
    /// on the screen meant a dozen reads while the pointer was moving.
    pub fn get(&self, path: &Path) -> Option<std::sync::Arc<IconImage>> {
        let key = path.to_string_lossy().to_ascii_lowercase();
        self.lookup(&key)
    }

    /// Learn the icon of a path: read it from the disk and keep it.
    ///
    /// This is called when the program is **opened** — the file is being
    /// opened anyway, so that is the moment to take its icon, and the next
    /// time the program comes up the row has an icon without reading
    /// anything.
    ///
    /// A path that is known already is left alone: no read, no write.
    pub fn learn(&self, path: &Path) {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if self.learned.lock().contains_key(&key) {
            // We know this one already. Reading it again would be a read for
            // nothing, and writing it again would be a write for nothing.
            return;
        }
        let mut pending = self.pending.lock();
        if !pending.insert(key) {
            // It is already on its way.
            return;
        }
        drop(pending);
        let _ = self.sender.send(path.to_string_lossy().to_string());
    }

    /// Number of cached entries.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A cache of icons that came as bytes rather than as a path.
///
/// The Store applications hand their icons over as PNG data. Decoding a PNG
/// is CPU work, so it happens on a background thread and the row shows its
/// letter badge until the pixels are there.
pub struct ByteIconCache {
    entries: Arc<Mutex<HashMap<String, Option<Arc<IconImage>>>>>,
    /// The textures of the icons, by key.
    ///
    /// The same as in the [`IconCache`]: the texture is made once and kept,
    /// instead of being uploaded again for every frame.
    textures: Arc<Mutex<HashMap<String, egui::TextureHandle>>>,
    /// Icons whose decoding has been asked for already.
    pending: Arc<Mutex<HashSet<String>>>,
    /// The queue the decoder thread reads from.
    sender: Sender<(String, Arc<Vec<u8>>)>,
}

impl ByteIconCache {
    /// Create a cache.
    pub fn new(ctx: egui::Context) -> Self {
        let entries = Arc::new(Mutex::new(HashMap::new()));
        let textures = Arc::new(Mutex::new(HashMap::<String, egui::TextureHandle>::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let (sender, receiver) = std::sync::mpsc::channel::<(String, Arc<Vec<u8>>)>();

        {
            let entries = entries.clone();
            let pending = pending.clone();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("icon-decoder-bytes".into())
                .spawn(move || {
                    while let Ok((key, bytes)) = receiver.recv() {
                        let decoded = decode_png(&bytes).map(Arc::new);
                        entries.lock().insert(key.clone(), decoded);
                        pending.lock().remove(&key);
                        ctx.request_repaint();
                    }
                })
                .ok();
        }

        Self {
            entries,
            textures,
            pending,
            sender,
        }
    }

    /// Get the icon for `bytes`, scheduling the decoding when needed.
    ///
    /// Returns `None` until the background thread has produced the pixels.
    pub fn get(&self, bytes: &[u8]) -> Option<Arc<IconImage>> {
        let key = bytes_key(bytes);
        if let Some(hit) = self.entries.lock().get(&key) {
            return hit.clone();
        }

        let mut pending = self.pending.lock();
        let fresh = pending.insert(key.clone());
        if fresh {
            let _ = self
                .sender
                .send((key, Arc::new(bytes.to_vec())));
        }
        drop(pending);

        // The decoder asks for the repaint when it has the pixels — see the
        // note in `IconCache::get`.
        None
    }

    /// The texture of the icon, made once and then kept.
    pub fn texture(&self, bytes: &[u8], ctx: &egui::Context) -> Option<egui::TextureHandle> {
        let image = self.get(bytes)?;
        let key = bytes_key(bytes);
        if let Some(handle) = self.textures.lock().get(&key) {
            return Some(handle.clone());
        }
        let handle = ctx.load_texture(
            "icon-bytes",
            to_color_image(&image),
            egui::TextureOptions::LINEAR,
        );
        self.textures.lock().insert(key, handle.clone());
        Some(handle)
    }
}

/// The key an icon is cached under.
fn bytes_key(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Decode an encoded image into raw RGBA pixels.
fn decode_png(bytes: &[u8]) -> Option<IconImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let rgba = image.to_rgba8();
    Some(IconImage {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

/// Extract a 32×32 icon from an executable or icon file.
///
/// Returns the raw RGBA pixels.
pub fn extract_icon(path: &Path, size: i32) -> Option<IconImage> {
    #[cfg(windows)]
    {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let mut large: HICON = std::ptr::null_mut();
            let mut small: HICON = std::ptr::null_mut();
            let count = ExtractIconExW(wide.as_ptr(), 0, &mut large, &mut small, 1);
            if count == 0 || large.is_null() {
                return None;
            }

            let image = render_icon(large, size);
            DestroyIcon(large);
            if !small.is_null() {
                DestroyIcon(small);
            }
            image
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (path, size);
        None
    }
}

/// What an icon lookup found.
struct Decoded {
    /// The pixels, when there are any.
    image: Option<std::sync::Arc<IconImage>>,
    /// True when the path turned out to be a folder.
    folder: bool,
}

/// The icon of a file or a folder.
///
/// Four things are tried, in this order:
///
/// * the icon of the folder, when the path is a folder — only the shell
///   knows which one that is,
/// * the icon that is in the file itself — an executable, an icon file,
/// * the picture itself, when the file is a picture,
/// * the icon of the program that opens the file — a text file gets the icon
///   of the editor the user chose for text files.
///
/// The last one is the interesting one: without it a text file would show a
/// letter, and the list would look like a wall of initials.
fn decode_icon(
    path: &Path,
    associated: &Mutex<HashMap<String, Option<std::sync::Arc<IconImage>>>>,
) -> Decoded {
    // A folder has no icon of its own: the shell draws it. It is also the
    // only kind of path without an extension, and asking the shell about
    // the "extension" of a folder would ask for the wrong icon — or for
    // none at all, which is what happened before this branch existed.
    if is_directory(path) {
        return Decoded {
            image: folder_icon(path, 32).map(std::sync::Arc::new),
            folder: true,
        };
    }
    if let Some(image) = extract_icon(path, 32) {
        return Decoded {
            image: Some(std::sync::Arc::new(image)),
            folder: false,
        };
    }
    if let Some(image) = picture_icon(path) {
        return Decoded {
            image: Some(std::sync::Arc::new(image)),
            folder: false,
        };
    }
    // Every file of a kind is opened by the same program, so the icon is the
    // same too — it is looked up once per kind of file, not once per file.
    let Some(key) = extension_key(path) else {
        return Decoded {
            image: None,
            folder: false,
        };
    };
    // **A shortcut and a program carry their own icon.** A shortcut shows
    // the icon of the program it points at, and every shortcut points at a
    // different program — so the first shortcut that is decoded would give
    // its icon to all the others (a Discord shortcut showing the icon of
    // Zed, because Zed's shortcut happened to be looked at first). The same
    // goes for a program: its icon is its own, not the icon of the kind.
    if carries_own_icon(&key) {
        let image = associated_icon(path, 32).map(std::sync::Arc::new);
        return Decoded {
            image,
            folder: false,
        };
    }
    // The shell is asked for the icon of the kind of file — but **without the
    // lock**: the call goes into the shell of the explorer, and it can take
    // milliseconds. Holding the lock while it happens blocks the drawing
    // thread, which asks for the very same lock for every row of the list.
    let cached = associated.lock().get(&key).cloned();
    if let Some(hit) = cached {
        return Decoded {
            image: hit,
            folder: false,
        };
    }
    let image = associated_icon(path, 32).map(std::sync::Arc::new);
    associated.lock().insert(key, image.clone());
    Decoded {
        image,
        folder: false,
    }
}

/// The kinds of file whose icon belongs to the file itself.
///
/// Their icons are not shared: one shortcut does not stand in for another,
/// and one program does not stand in for another.
fn carries_own_icon(extension: &str) -> bool {
    matches!(
        extension,
        "lnk" | "url" | "exe" | "dll" | "ico" | "cur" | "scr" | "bat" | "cmd"
    )
}

/// True when the path is a folder.
///
/// The answer is a question about the disk, so it is asked on the thread
/// that decodes the icons — never on the one that draws the window.
fn is_directory(path: &Path) -> bool {
    std::fs::metadata(path).map(|meta| meta.is_dir()).unwrap_or(false)
}

/// The icon of a folder.
///
/// Only the shell draws the folders, and it knows two things about them that
/// nothing else does: the plain folder icon, and the icon that a
/// `desktop.ini` has set for one folder.
///
/// The shell has to be asked with the disk: the attributes of a path say
/// nothing about the icon of a folder, and asking without the disk gives
/// the blank icon of a file without a name — measured, not guessed. The
/// trick of a trailing backslash does not change that.
fn folder_icon(path: &Path, size: i32) -> Option<IconImage> {
    shell_icon(path, SHGFI_ICON, size)
}

/// The icon of the program that opens this file.
///
/// The shell knows which program that is, and it answers with the one the
/// user has chosen — so a text file shows the icon of their editor and not
/// the icon of the system one.
fn associated_icon(path: &Path, size: i32) -> Option<IconImage> {
    shell_icon(path, SHGFI_ICON | SHGFI_USEFILEATTRIBUTES, size)
}

/// Ask the shell for the icon of a path.
///
/// The flags say how the shell should look: `SHGFI_USEFILEATTRIBUTES` keeps
/// it from touching the disk, which is what makes it possible to ask about
/// a path that is not there at all.
pub fn shell_icon(path: &Path, flags: u32, size: i32) -> Option<IconImage> {
    #[cfg(windows)]
    {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let mut info: SHFILEINFO = std::mem::zeroed();
            let ok = SHGetFileInfoW(
                wide.as_ptr(),
                0,
                &mut info,
                std::mem::size_of::<SHFILEINFO>() as u32,
                flags,
            );
            if ok == 0 || info.hIcon.is_null() {
                return None;
            }
            let image = render_icon(info.hIcon, size);
            DestroyIcon(info.hIcon);
            image
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (path, flags, size);
        None
    }
}

/// The extension of a path, in lower case and without the dot.
fn extension_key(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    if extension.is_empty() {
        None
    } else {
        Some(extension)
    }
}

/// The picture itself, when the file is a picture.
pub fn picture_icon(path: &Path) -> Option<IconImage> {
    let extension = extension_key(path)?;
    if !matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "tif" | "tiff" | "ico" | "svg"
    ) {
        return None;
    }
    let image = image::open(path).ok()?;
    let rgba = image.to_rgba8();
    Some(IconImage {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

/// Render an icon into a 32-bit bitmap.
///
/// The icon is drawn by the Windows icon renderer into an off-screen
/// 32-bit DIB section. Reading the colour bitmap with `GetDIBits` instead
/// would lose the alpha channel: `GetDIBits` does not preserve it for
/// 32-bit icons, and their 1-bit `hbmMask` is opaque everywhere, so the
/// transparent corners of the icon would end up as a black square.
fn render_icon(hicon: HICON, size: i32) -> Option<IconImage> {
    unsafe {
        // The icon's own size comes from its colour bitmap.
        let mut info = ICONINFO {
            fIcon: 0,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: std::ptr::null_mut(),
            hbmColor: std::ptr::null_mut(),
        };
        if GetIconInfo(hicon, &mut info) == 0 || info.hbmColor.is_null() {
            return None;
        }
        let mut bmp = BITMAP {
            bmType: 0,
            bmWidth: 0,
            bmHeight: 0,
            bmWidthBytes: 0,
            bmPlanes: 1,
            bmBitCount: 0,
            bmBits: std::ptr::null_mut(),
        };
        GetObjectW(
            info.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bmp as *mut BITMAP as *mut core::ffi::c_void,
        );
        let src_w = bmp.bmWidth.max(1);
        let src_h = bmp.bmHeight.abs().max(1);

        // The colour bitmap of a modern (32-bit) icon is a DIB section, and
        // its pixels carry the alpha channel. `GetDIBits` does not preserve
        // that channel, so the bits are read directly whenever the bitmap
        // exposes them.
        let direct = if !bmp.bmBits.is_null() && bmp.bmBitCount == 32 {
            Some((bmp.bmBits, bmp.bmWidthBytes))
        } else {
            None
        };

        let mut pixels = vec![0u8; (src_w as usize) * (src_h as usize) * 4];

        match direct {
            Some((bits, stride)) => {
                log::info!(
                    "direct path: bmBits ok, stride={stride} bmp={src_w}x{src_h} bitcount={}",
                    bmp.bmBitCount
                );
                // The rows of a device bitmap are stored bottom-up.
                let stride = if stride > 0 {
                    stride as usize
                } else {
                    (src_w as usize) * 4
                };
                for y in 0..src_h as usize {
                    let src_row = src_h as usize - 1 - y;
                    let from = bits.add(src_row * stride) as *const u8;
                    let to = (y * src_w as usize) * 4;
                    std::ptr::copy_nonoverlapping(from, pixels[to..].as_mut_ptr(), src_w as usize * 4);
                }
            }
            None => {
                // An older icon without a DIB section: read it with GetDIBits
                // and rebuild the alpha from the 1-bit mask.
                let screen_dc = GetDC(std::ptr::null_mut());
                if screen_dc.is_null() {
                    return None;
                }
                let mut bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: src_w,
                        biHeight: -src_h,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    bmiColors: [0; 3],
                };
                let got = GetDIBits(
                    screen_dc,
                    info.hbmColor,
                    0,
                    src_h as u32,
                    pixels.as_mut_ptr() as *mut core::ffi::c_void,
                    &mut bmi,
                    DIB_RGB_COLORS,
                );
                ReleaseDC(std::ptr::null_mut(), screen_dc);
                if got == 0 {
                    return None;
                }

                // Rebuild the alpha from the 1-bit mask — but only when the
                // colour bitmap does not carry an alpha channel itself.
                // A 32-bit icon's colour bitmap has a real alpha channel, and
                // `GetDIBits` does preserve it; overwriting it with the
                // (opaque everywhere) mask would turn the transparent
                // corners of the icon into a black square.
                let mut alpha_is_present = false;
                if bmp.bmBitCount == 32 {
                    alpha_is_present = pixels.chunks_exact(4).any(|p| p[3] != 0);
                }

                let mut mask: Vec<u8> = Vec::new();
                let mut mask_w = 0usize;
                let mut mask_h = 0usize;
                if !info.hbmMask.is_null() {
                    let mut mbmp = BITMAP {
                        bmType: 0,
                        bmWidth: 0,
                        bmHeight: 0,
                        bmWidthBytes: 0,
                        bmPlanes: 1,
                        bmBitCount: 0,
                        bmBits: std::ptr::null_mut(),
                    };
                    GetObjectW(
                        info.hbmMask,
                        std::mem::size_of::<BITMAP>() as i32,
                        &mut mbmp as *mut BITMAP as *mut core::ffi::c_void,
                    );
                    mask_w = mbmp.bmWidth.max(0) as usize;
                    mask_h = mbmp.bmHeight.abs().max(0) as usize;
                    if mask_w > 0 && mask_h > 0 {
                        let mut mbmi = BITMAPINFO {
                            bmiHeader: BITMAPINFOHEADER {
                                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                                biWidth: mask_w as i32,
                                biHeight: -(mask_h as i32),
                                biPlanes: 1,
                                biBitCount: 1,
                                biCompression: BI_RGB,
                                biSizeImage: 0,
                                biXPelsPerMeter: 0,
                                biYPelsPerMeter: 0,
                                biClrUsed: 0,
                                biClrImportant: 0,
                            },
                            bmiColors: [0; 3],
                        };
                        let stride = ((mask_w + 31) / 32) * 4;
                        let mut raw = vec![0u8; stride * mask_h];
                        let screen_dc = GetDC(std::ptr::null_mut());
                        if !screen_dc.is_null() {
                            let got = GetDIBits(
                                screen_dc,
                                info.hbmMask,
                                0,
                                mask_h as u32,
                                raw.as_mut_ptr() as *mut core::ffi::c_void,
                                &mut mbmi,
                                DIB_RGB_COLORS,
                            );
                            ReleaseDC(std::ptr::null_mut(), screen_dc);
                            if got != 0 {
                                mask = raw;
                            }
                        }
                    }
                }

                let mask_stride = ((mask_w + 31) / 32) * 4;
                if !alpha_is_present {
                    for y in 0..src_h as usize {
                        for x in 0..src_w as usize {
                            let p = (y * src_w as usize + x) * 4;
                            let opaque = if mask.is_empty() {
                                true
                            } else if y < mask_h && x < mask_w {
                                let byte = mask[y * mask_stride + x / 8];
                                (byte >> (7 - (x % 8))) & 1 != 0
                            } else {
                                true
                            };
                            pixels[p + 3] = if opaque { 255 } else { 0 };
                        }
                    }
                }
            }
        }

        // BGRA -> RGBA.
        for chunk in pixels.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }
        // --- scale to the requested size ---------------------------------------
        let out_w = size.max(1) as usize;
        let out_h = size.max(1) as usize;
        let mut out = vec![0u8; out_w * out_h * 4];
        for y in 0..out_h {
            for x in 0..out_w {
                let sx = (x as i64 * src_w as i64 / out_w as i64).min(src_w as i64 - 1) as usize;
                let sy = (y as i64 * src_h as i64 / out_h as i64).min(src_h as i64 - 1) as usize;
                let s = (sy * src_w as usize + sx) * 4;
                let d = (y * out_w + x) * 4;
                out[d..d + 4].copy_from_slice(&pixels[s..s + 4]);
            }
        }

        Some(IconImage {
            width: out_w,
            height: out_h,
            rgba: out,
        })
    }
}
/// Extract an icon and write it to a PNG — a small debugging helper.
///
/// The icon is looked for the same way the launcher looks for it: the folder
/// icon, then the one in the file, then the picture itself, then the icon of
/// the program that opens the file.
pub fn extract_icon_to_png(path: &Path, size: i32, out: &Path) -> Option<()> {
    let (image, source) = if is_directory(path) {
        (folder_icon(path, size)?, "文件夹")
    } else if let Some(image) = extract_icon(path, size) {
        (image, "文件自带")
    } else if let Some(image) = picture_icon(path) {
        (image, "图片本身")
    } else if let Some(image) = associated_icon(path, size) {
        (image, "打开它的程序")
    } else {
        log::info!("没有找到图标: {}", path.display());
        return None;
    };
    log::info!("图标来自: {source}");
    let transparent = image.rgba.chunks_exact(4).filter(|p| p[3] == 0).count();
    let opaque = image.rgba.chunks_exact(4).filter(|p| p[3] == 255).count();
    let partial = image.rgba.chunks_exact(4).filter(|p| p[3] != 0 && p[3] != 255).count();
    log::info!(
        "alpha histogram: transparent={transparent} opaque={opaque} partial={partial} ({} px)",
        image.width * image.height
    );
    let img = image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.rgba)?;
    img.save(out).ok()?;
    Some(())
}

/// Launch a file or URL with the default handler.
pub fn shell_open(target: &str) {
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        );
    }
}

/// Launch a program with arguments.
pub fn shell_open_with(target: &str, arguments: &str) {
    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let args: Vec<u16> = arguments.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            args.as_ptr(),
            std::ptr::null(),
            1,
        );
    }
}

/// Run a console program without a console window.
///
/// The shell commands of the launcher are run through `cmd.exe`. Started the
/// usual way, it opens a black window that takes the focus away from the
/// launcher — which then hides itself, and the user sees the window
/// disappear. `CREATE_NO_WINDOW` says that no window is wanted.
pub fn shell_run_console(program: &str, arguments: &str) {
    let mut command = format!("\"{}\" {}", program, arguments);
    let mut wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let program_wide: Vec<u16> = program.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut startup: StartupInfoW = std::mem::zeroed();
        startup.cb = std::mem::size_of::<StartupInfoW>() as u32;
        let mut info: ProcessInformation = std::mem::zeroed();
        let ok = CreateProcessW(
            program_wide.as_ptr(),
            wide.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &startup,
            &mut info,
        );
        if ok != 0 {
            // The process is on its own; the handles are of no use any more.
            CloseHandle(info.hProcess);
            CloseHandle(info.hThread);
        } else {
            log::warn!("the command could not be started: {command}");
        }
    }
    command.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store that keeps to itself, so that a test never sees the icons the
    /// user has learned — and never writes into the user's own data.
    fn scratch_store(name: &str) -> (PathBuf, std::sync::Arc<IconStore>) {
        let folder = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&folder);
        (folder.clone(), std::sync::Arc::new(IconStore::in_folder(folder)))
    }

    #[test]
    fn an_icon_that_was_learned_comes_back() {
        let (folder, store) = scratch_store("ideas-icon-store-test");
        let image = IconImage {
            width: 2,
            height: 2,
            rgba: (0u8..16).collect(),
        };
        store.save(Path::new("C:\\some\\program.exe"), &image);

        let back = store.load();
        assert_eq!(back.len(), 1, "the icon we learned has to come back");
        assert_eq!(back[0].0, "c:\\some\\program.exe");
        assert_eq!(back[0].1.rgba, image.rgba);
        assert_eq!(back[0].1.width, 2);
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// An icon that is already in the store must not be written a second
    /// time — the disk should not be worn out for nothing.
    #[test]
    fn an_icon_in_the_store_is_not_written_again() {
        let (folder, store) = scratch_store("ideas-icon-store-nowrite-test");
        let image = IconImage {
            width: 1,
            height: 1,
            rgba: vec![7; 4],
        };
        let count = |f: &Path| std::fs::read_dir(f).map(|d| d.flatten().count()).unwrap_or(0);

        store.save(Path::new("C:\\some\\program.exe"), &image);
        let before = count(&folder);
        assert_eq!(before, 1, "the icon we learned has to be written down");

        store.save(Path::new("C:\\some\\program.exe"), &image);
        assert_eq!(
            count(&folder),
            before,
            "the icon must not be written a second time"
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A learned icon must not be read a second time: the program was opened
    /// once, its icon was taken, and that is enough for ever.
    #[test]
    fn a_learned_icon_is_never_read_again() {
        let (folder, store) = scratch_store("ideas-icon-learned-test");
        let image = IconImage {
            width: 2,
            height: 2,
            rgba: (0u8..16).collect(),
        };
        store.save(Path::new("C:\\some\\program.exe"), &image);
        let cache = IconCache::with_store(4, egui::Context::default(), store);

        // It is there, without a single read.
        assert!(cache.get(Path::new("C:\\some\\program.exe")).is_some());

        // And learning it again does nothing at all.
        cache.learn(Path::new("C:\\some\\program.exe"));
        assert!(cache.get(Path::new("C:\\some\\program.exe")).is_some());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_cache_starts_empty() {
        let (folder, store) = scratch_store("ideas-icon-empty-test");
        let cache = IconCache::with_store(4, egui::Context::default(), store);
        assert!(cache.is_empty());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn missing_file_yields_no_icon() {
        let (folder, store) = scratch_store("ideas-icon-missing-test");
        let cache = IconCache::with_store(4, egui::Context::default(), store);
        assert!(
            cache
                .get(Path::new(r"C:\definitely\not\here.exe"))
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn cache_respects_capacity() {
        let (folder, store) = scratch_store("ideas-icon-capacity-test");
        let cache = IconCache::with_store(2, egui::Context::default(), store);
        for i in 0..5 {
            cache.get(Path::new(&format!("C:\\nope\\{i}.exe")));
        }
        assert!(cache.len() <= 2);
        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A shortcut shows the icon of the program it points at, and every
    /// shortcut points at a different program — so they must not share one
    /// icon. The same goes for a program: its icon is its own.
    #[test]
    fn shortcuts_and_programs_carry_their_own_icon() {
        assert!(carries_own_icon("lnk"));
        assert!(carries_own_icon("url"));
        assert!(carries_own_icon("exe"));
        // The kinds of file that are opened by a program *do* share one.
        assert!(!carries_own_icon("txt"));
        assert!(!carries_own_icon("md"));
        assert!(!carries_own_icon("pdf"));
    }
}
