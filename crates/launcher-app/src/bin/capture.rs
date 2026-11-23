//! Capture the launcher window's own rendered surface.
//!
//! `PrintWindow` with `PW_RENDERFULLCONTENT` reads the window's own buffer, so
//! the result is not affected by other windows that happen to overlap it.
//!
//! Run with: `cargo run --release -p ideas-launcher --bin capture -- <out.png>`

use std::ffi::c_void;

use ideas_launcher::winapi;

/// Write the window with the given title to a PNG file.
fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "launcher-window.png".to_string());
    let title = std::env::args().nth(2).unwrap_or_else(|| "IDEAS Launcher".into());

    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = unsafe { winapi::FindWindowW(std::ptr::null(), wide.as_ptr()) };
    if hwnd.is_null() {
        eprintln!("window '{title}' not found");
        std::process::exit(1);
    }
    println!("window handle: {hwnd:p}");

    match capture(hwnd) {
        Some((w, h, rgba)) => {
            let path = std::path::Path::new(&out);
            let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
                .expect("buffer size mismatch");
            img.save(path).expect("failed to write png");
            println!("wrote {out} ({w}x{h})");
        }
        None => {
            eprintln!("capture failed");
            std::process::exit(2);
        }
    }
}

/// Render the window into an RGBA buffer.
fn capture(hwnd: winapi::HWND) -> Option<(usize, usize, Vec<u8>)> {
    unsafe {
        let mut rect = winapi::RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if winapi::GetWindowRect(hwnd, &mut rect) == 0 {
            return None;
        }
        let width = (rect.right - rect.left).max(1);
        let height = (rect.bottom - rect.top).max(1);

        let window_dc = winapi::GetWindowDC(hwnd);
        if window_dc.is_null() {
            return None;
        }
        let mem_dc = winapi::CreateCompatibleDC(window_dc);
        if mem_dc.is_null() {
            winapi::ReleaseDC(hwnd, window_dc);
            return None;
        }

        let mut bmi = winapi::BITMAPINFO {
            bmiHeader: winapi::BITMAPINFOHEADER {
                biSize: std::mem::size_of::<winapi::BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative height: rows are stored top-down.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: winapi::BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [0; 3],
        };

        let mut bits: *mut c_void = std::ptr::null_mut();
        let hbmp = winapi::CreateDIBSection(
            mem_dc,
            &mut bmi,
            winapi::DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if hbmp.is_null() || bits.is_null() {
            winapi::DeleteDC(mem_dc);
            winapi::ReleaseDC(hwnd, window_dc);
            return None;
        }

        let previous = winapi::SelectObject(mem_dc, hbmp as winapi::HGDIOBJ);
        // 2 = PW_RENDERFULLCONTENT, required for composited windows.
        //
        // It does not work for an OpenGL surface though, so the screen is
        // used as the source instead: the launcher is always on top and in
        // the foreground, so its pixels are on screen.
        let screen_dc = winapi::GetDC(std::ptr::null_mut());
        let ok = if !screen_dc.is_null() {
            let copied = winapi::BitBlt(
                mem_dc,
                0,
                0,
                width,
                height,
                screen_dc,
                rect.left,
                rect.top,
                winapi::SRCCOPY,
            );
            winapi::ReleaseDC(std::ptr::null_mut(), screen_dc);
            copied
        } else {
            winapi::PrintWindow(hwnd, mem_dc, 2)
        };
        winapi::SelectObject(mem_dc, previous);
        winapi::DeleteObject(hbmp as winapi::HGDIOBJ);
        winapi::DeleteDC(mem_dc);
        winapi::ReleaseDC(hwnd, window_dc);

        if ok == 0 {
            eprintln!("PrintWindow failed");
            return None;
        }

        let stride = width as usize * 4;
        let mut buffer = vec![0u8; stride * height as usize];
        std::ptr::copy_nonoverlapping(bits as *const u8, buffer.as_mut_ptr(), buffer.len());

        // BGRA -> RGBA.
        for chunk in buffer.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        Some((width as usize, height as usize, buffer))
    }
}
