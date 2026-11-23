//! Probe the different ways of getting a 32-bit icon with its alpha channel.

use ideas_launcher::winapi::*;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;

fn variants(path: &str) {
    let wide: Vec<u16> = std::path::Path::new(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut large: HICON = std::ptr::null_mut();
        let mut small: HICON = std::ptr::null_mut();
        let count = ExtractIconExW(wide.as_ptr(), 0, &mut large, &mut small, 1);
        println!("ExtractIconEx count={count} large={large:p} small={small:p}");
        if large.is_null() {
            return;
        }

        let mut info = ICONINFO {
            fIcon: 0,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: std::ptr::null_mut(),
            hbmColor: std::ptr::null_mut(),
        };
        let gi = GetIconInfo(large, &mut info);
        println!("GetIconInfo={gi} hbmColor={:p} hbmMask={:p}", info.hbmColor, info.hbmMask);

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
            &mut bmp as *mut BITMAP as *mut c_void,
        );
        println!(
            "colour bitmap: {}x{} bitcount={} bmBits={:p} widthBytes={}",
            bmp.bmWidth,
            bmp.bmHeight,
            bmp.bmBitCount,
            bmp.bmBits,
            bmp.bmWidthBytes
        );

        // How many pixels does GetDIBits give an alpha for?
        let screen = GetDC(std::ptr::null_mut());
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bmp.bmWidth,
                biHeight: -bmp.bmHeight.abs(),
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
        let mut buf = vec![0u8; (bmp.bmWidth * bmp.bmHeight.abs()) as usize * 4];
        let got = GetDIBits(
            screen,
            info.hbmColor,
            0,
            bmp.bmHeight.abs() as u32,
            buf.as_mut_ptr() as *mut c_void,
            &mut bmi,
            DIB_RGB_COLORS,
        );
        let nonzero = buf.chunks_exact(4).filter(|p| p[3] != 0).count();
        let black = buf.chunks_exact(4).filter(|p| p[0] == 0 && p[1] == 0 && p[2] == 0).count();
        println!("GetDIBits got={got} alpha!=0 in {nonzero} px, black in {black} px");

        // Now the DrawIconEx variants.
        for (label, use_null_dc) in [("screen-dc", false), ("null-dc", true)] {
            for (flag_label, flags) in [("DI_NORMAL", DI_NORMAL), ("0", 0)] {
                let mem_dc = if use_null_dc {
                    CreateCompatibleDC(std::ptr::null_mut())
                } else {
                    CreateCompatibleDC(screen)
                };
                let mut bmi2 = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: 32,
                        biHeight: -32,
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
                let mut bits: *mut c_void = std::ptr::null_mut();
                let hbmp = CreateDIBSection(
                    mem_dc,
                    &mut bmi2,
                    DIB_RGB_COLORS,
                    &mut bits,
                    std::ptr::null_mut(),
                    0,
                );
                let old = SelectObject(mem_dc, hbmp as HGDIOBJ);
                std::ptr::write_bytes(bits, 0, 32 * 32 * 4);
                let ok = DrawIconEx(
                    mem_dc,
                    0,
                    0,
                    32,
                    32,
                    large,
                    std::ptr::null_mut(),
                    flags,
                );
                let mut transparent = 0;
                let mut opaque = 0;
                if ok != 0 {
                    let mut px = vec![0u8; 32 * 32 * 4];
                    std::ptr::copy_nonoverlapping(bits as *const u8, px.as_mut_ptr(), px.len());
                    for p in px.chunks_exact(4) {
                        if p[3] == 0 {
                            transparent += 1;
                        } else if p[3] == 255 {
                            opaque += 1;
                        }
                    }
                }
                println!(
                    "  {label} {flag_label}: hbmp={:p} old={:p} bits={:p} ok={ok} transparent={transparent} opaque={opaque}",
                    hbmp, old, bits
                );
                SelectObject(mem_dc, old);
                DeleteObject(hbmp as HGDIOBJ);
                DeleteDC(mem_dc);
            }
        }
        ReleaseDC(std::ptr::null_mut(), screen);
    }
}

fn dump(data: &[u8], from: usize, to: usize) {
    let to = to.min(data.len());
    let mut at = from;
    while at < to {
        let end = (at + 16).min(to);
        let row = &data[at..end];
        let hex: Vec<String> = row.iter().map(|b| format!("{b:02X}")).collect();
        let ascii: String = row
            .iter()
            .map(|b| {
                if (0x20..0x7F).contains(b) {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {at:08X}  {:<47}  {ascii}", hex.join(" "));
        at = end;
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe".to_string());

    // For a shortcut: show what our own parser recovered.
    if path.to_lowercase().ends_with(".lnk") {
        let data = std::fs::read(&path).expect("read");
        println!("file size = {}", data.len());
        let header_size = u32::from_le_bytes(data[0..4].try_into().unwrap());
        let flags = u32::from_le_bytes(data[20..24].try_into().unwrap());
        println!("header size = {header_size} (0x{header_size:X})");
        println!(
            "flags = 0x{flags:08X}  hasIDList={} hasLinkInfo={} hasName={} hasRelPath={} hasArgs={} hasIcon={} unicode={}",
            flags & 0x01 != 0,
            flags & 0x02 != 0,
            flags & 0x04 != 0,
            flags & 0x08 != 0,
            flags & 0x20 != 0,
            flags & 0x40 != 0,
            flags & 0x80 != 0
        );
        println!("--- bytes 0x4C..0x80 ---");
        dump(&data, 0x4C, 0x80);
        println!("--- bytes around the LinkInfo ---");
        // The ID list is the first structure after the header.
        let id_size = u16::from_le_bytes(data[0x4C..0x4E].try_into().unwrap()) as usize;
        let link_info_at = 0x4C + id_size;
        println!("id list size = {id_size} -> LinkInfo at 0x{link_info_at:X}");
        dump(&data, link_info_at, link_info_at + 0x60);
        let link = launcher_index::lnk::parse(&data);
        println!("parsed target : {:?}", link.as_ref().and_then(|l| l.target_path.clone()));
        println!("parsed icon   : {:?}", link.as_ref().and_then(|l| l.icon_location.clone()));
        println!("parsed name   : {:?}", link.as_ref().and_then(|l| l.name.clone()));
        println!(
            "parsed descr  : {:?}",
            link.as_ref().and_then(|l| l.description.clone())
        );
    }

    variants(&path);
}
