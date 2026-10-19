//! Embeds the exe icon, version info and manifest. The icon (a red "recording"
//! disc, matching the tray) is generated here, so the repo carries no binary assets.

use std::io::Write;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/meetrec.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::env::var("OUT_DIR").unwrap();
    let ico = Path::new(&out).join("meetrec.ico");
    write_ico(&ico, &[16, 32, 48, 256]).expect("write icon");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap())
        .set("FileDescription", "MeetRec meeting recorder")
        .set("ProductName", "MeetRec")
        .set_manifest_file("assets/meetrec.manifest");
    res.compile().expect("compile Windows resources");
}

/// RGBA disc, anti-aliased, red on transparent.
fn disc(size: u32) -> Vec<u8> {
    let c = size as f32 / 2.0;
    let r = c * 0.875;
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let a = (r + 0.5 - d).clamp(0.0, 1.0);
            px.extend_from_slice(&[220, 40, 40, (a * 255.0) as u8]);
        }
    }
    px
}

/// ICO with one 32-bit BMP image per size.
fn write_ico(path: &Path, sizes: &[u32]) -> std::io::Result<()> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_entry(s, &disc(s))).collect();
    let mut f = std::fs::File::create(path)?;
    f.write_all(&[0, 0, 1, 0])?;
    f.write_all(&(sizes.len() as u16).to_le_bytes())?;
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        f.write_all(&[dim, dim, 0, 0])?;
        f.write_all(&1u16.to_le_bytes())?; // planes
        f.write_all(&32u16.to_le_bytes())?; // bpp
        f.write_all(&(img.len() as u32).to_le_bytes())?;
        f.write_all(&offset.to_le_bytes())?;
        offset += img.len() as u32;
    }
    for img in &images {
        f.write_all(img)?;
    }
    Ok(())
}

/// BITMAPINFOHEADER + bottom-up BGRA pixels + an all-zero AND mask.
fn bmp_entry(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(size as i32).to_le_bytes());
    b.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // XOR + AND height
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]); // compression, sizes, resolution, palette
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            b.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    let mask_row = size.div_ceil(32) * 4;
    b.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    b
}
