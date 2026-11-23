//! Generate the application icon.
//!
//! The icon is drawn from scratch — a rounded square in the accent colour
//! with a magnifying glass — and written as a multi-size `.ico` file, which
//! the build script embeds into the executable.
//!
//! The container is written by hand rather than with a codec, because the
//! classic uncompressed layout (a DIB per size) is understood by every
//! Windows version and by the shell's icon cache.
//!
//! Run with: `cargo run --release -p ideas-launcher --bin make-icon -- <out.ico>`

use image::{Rgba, RgbaImage};

/// The accent colour of the default theme.
const ACCENT: [u8; 4] = [0x3D, 0x7D, 0xF6, 0xFF];
/// The colour of the magnifying glass.
const GLASS: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// The size the icon is drawn at before it is scaled down.
const SUPERSAMPLE: u32 = 4;

/// The sizes the icon file holds.
const SIZES: [u32; 6] = [16, 24, 32, 48, 64, 256];

/// Draw the icon at `size` pixels.
fn draw(size: u32) -> RgbaImage {
    let s = size * SUPERSAMPLE;
    let mut image = RgbaImage::from_pixel(s, s, Rgba([0, 0, 0, 0]));

    // --- the rounded square ----------------------------------------------
    let radius = (s as f32 * 0.24) as i32;
    for y in 0..s {
        for x in 0..s {
            if corner_inset(x as i32, y as i32, s as i32, radius).is_some() {
                image.put_pixel(x, y, Rgba(ACCENT));
                // A soft vertical gradient makes the badge look less flat.
                let t = y as f32 / s as f32;
                let shade = 1.0 - 0.18 * t;
                let p = image.get_pixel_mut(x, y);
                p.0[0] = (p.0[0] as f32 * shade) as u8;
                p.0[1] = (p.0[1] as f32 * shade) as u8;
                p.0[2] = (p.0[2] as f32 * shade) as u8;
            }
        }
    }

    // --- the magnifying glass --------------------------------------------
    let cx = s as f32 * 0.43;
    let cy = s as f32 * 0.42;
    let ring_radius = s as f32 * 0.185;
    let ring_width = s as f32 * 0.075;
    let handle_start = (cx + ring_radius * 0.72, cy + ring_radius * 0.72);
    let handle_end = (s as f32 * 0.80, s as f32 * 0.79);
    let handle_width = s as f32 * 0.085;

    for y in 0..s {
        for x in 0..s {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = px - cx;
            let dy = py - cy;
            let distance = (dx * dx + dy * dy).sqrt();
            let on_ring = (distance - ring_radius).abs() <= ring_width / 2.0;

            let (hx, hy) = (handle_end.0 - handle_start.0, handle_end.1 - handle_start.1);
            let length_sq = hx * hx + hy * hy;
            let t = (((px - handle_start.0) * hx + (py - handle_start.1) * hy) / length_sq).clamp(0.0, 1.0);
            let nx = handle_start.0 + t * hx;
            let ny = handle_start.1 + t * hy;
            let on_handle = ((px - nx).powi(2) + (py - ny).powi(2)).sqrt() <= handle_width / 2.0;

            if on_ring || on_handle {
                image.put_pixel(x, y, Rgba(GLASS));
            }
        }
    }

    image::imageops::resize(&image, size, size, image::imageops::FilterType::Lanczos3)
}

/// Whether a pixel falls inside the rounded square.
fn corner_inset(x: i32, y: i32, size: i32, radius: i32) -> Option<()> {
    let (cx, cy) = match (x, y) {
        (x, y) if x < radius && y < radius => (radius, radius),
        (x, y) if x >= size - radius && y < radius => (size - radius - 1, radius),
        (x, y) if x < radius && y >= size - radius => (radius, size - radius - 1),
        (x, y) if x >= size - radius && y >= size - radius => (size - radius - 1, size - radius - 1),
        _ => return None,
    };
    let dx = (x - cx) as f32;
    let dy = (y - cy) as f32;
    if dx * dx + dy * dy <= (radius * radius) as f32 {
        Some(())
    } else {
        None
    }
}

/// The image data of one size: a DIB with the colour bitmap and the mask.
fn dib(image: &RgbaImage) -> Vec<u8> {
    let w = image.width() as usize;
    let h = image.height() as usize;
    let xor_size = w * h * 4;
    let and_stride = ((w + 31) / 32) * 4;

    let mut out = Vec::with_capacity(40 + xor_size + and_stride * h);

    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    // The height covers the colour bitmap *and* the mask.
    out.extend_from_slice(&((h * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bit count
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(xor_size as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // x pixels per metre
    out.extend_from_slice(&0u32.to_le_bytes()); // y pixels per metre
    out.extend_from_slice(&0u32.to_le_bytes()); // colours used
    out.extend_from_slice(&0u32.to_le_bytes()); // important colours

    // The colour bitmap, bottom-up, in BGRA.
    for y in (0..h).rev() {
        for x in 0..w {
            let p = image.get_pixel(x as u32, y as u32);
            out.push(p.0[2]);
            out.push(p.0[1]);
            out.push(p.0[0]);
            out.push(p.0[3]);
        }
    }

    // The mask: one bit per pixel, rows padded to four bytes.
    for y in (0..h).rev() {
        let mut row = vec![0u8; and_stride];
        for x in 0..w {
            let p = image.get_pixel(x as u32, y as u32);
            if p.0[3] == 0 {
                row[x / 8] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }

    out
}

/// Write the icon file.
fn write_ico(path: &str, images: &[RgbaImage]) -> std::io::Result<()> {
    let mut entries: Vec<Vec<u8>> = Vec::new();
    let mut payloads: Vec<Vec<u8>> = Vec::new();
    let mut offset = 6 + 16 * images.len();

    for image in images {
        let payload = dib(image);
        let w = image.width();
        let h = image.height();

        let mut entry = Vec::with_capacity(16);
        entry.push(if w >= 256 { 0 } else { w as u8 });
        entry.push(if h >= 256 { 0 } else { h as u8 });
        entry.push(0); // colour count
        entry.push(0); // reserved
        entry.extend_from_slice(&1u16.to_le_bytes()); // planes
        entry.extend_from_slice(&32u16.to_le_bytes()); // bit count
        entry.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        entry.extend_from_slice(&(offset as u32).to_le_bytes());

        offset += payload.len();
        entries.push(entry);
        payloads.push(payload);
    }

    let mut file = Vec::new();
    file.extend_from_slice(&0u16.to_le_bytes()); // reserved
    file.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    file.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for entry in &entries {
        file.extend_from_slice(entry);
    }
    for payload in &payloads {
        file.extend_from_slice(payload);
    }

    std::fs::write(path, file)
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "app.ico".to_string());

    let images: Vec<RgbaImage> = SIZES.iter().map(|size| draw(*size)).collect();
    write_ico(&out, &images).expect("write the icon file");

    // A PNG next to it, so the icon can be looked at without an icon viewer.
    if let Some(stem) = out.strip_suffix(".ico") {
        let png = format!("{stem}.png");
        images[images.len() - 1]
            .save(&png)
            .expect("write the preview");
        println!("wrote {png}");
    }

    println!("wrote {out} with {} sizes", images.len());
}
