//! Try the video player on a file.
//!
//! Run with:
//! `cargo run --release -p ideas-launcher --bin video-probe -- <video>`
//!
//! It prints what is known about the video, plays it for a few seconds and
//! says how many pictures came out of it.
//!
//! The FFmpeg programs are looked for in `runtime\ffmpeg` next to the program,
//! on the path, and in the directory the environment variable `IDEAS_FFMPEG`
//! points at.

use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: video-probe <video>");
        std::process::exit(2);
    };
    let path = std::path::PathBuf::from(&input);

    println!("file: {}", path.display());
    match ideas_launcher::video::ffmpeg_dir() {
        Some(dir) => println!("ffmpeg: {}", dir.display()),
        None => println!("ffmpeg: 没找到"),
    }

    let started = Instant::now();
    match ideas_launcher::video::probe(&path) {
        Some(info) => {
            println!("probe: {info:?}");
            println!("probe took {:?}", started.elapsed());
        }
        None => println!("probe: 什么也没问出来"),
    }

    // Play it and count the pictures.
    let ctx = egui::Context::default();
    let player = ideas_launcher::video::VideoPlayer::new(&path, ctx, None);
    // A big video is not started on its own; the probe starts it.
    if !player.is_playing() {
        player.play();
    }
    let mut pictures = 0usize;
    let mut last_position = 0u64;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(6) {
        std::thread::sleep(Duration::from_millis(100));
        if let Some(error) = player.error() {
            println!("error: {error}");
            return;
        }
        if player.frame().is_some() && player.position() != last_position {
            last_position = player.position();
            pictures += 1;
        }
    }
    println!(
        "pictures: {pictures}  position: {} ms  duration: {:?}  playing: {}",
        player.position(),
        player.duration(),
        player.is_playing()
    );
}
