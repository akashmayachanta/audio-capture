//! Embeds the exe icon, version info and manifest. The icon (the AudCap mark in
//! red, `src/icon.rs`, as in the tray) is generated here, so the repo carries no binary assets.
//! Also stages the Svelte app (`frontend/dist`) for `include_dir!`, or a placeholder
//! page when it has not been built (so `cargo test` works without npm).

use std::io::Write;
use std::path::Path;

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/audcap.manifest");
    println!("cargo:rerun-if-changed=src/icon.rs");
    stage_frontend();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::env::var("OUT_DIR").unwrap();
    let ico = Path::new(&out).join("audcap.ico");
    write_ico(&ico, &[16, 32, 48, 256]).expect("write icon");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap())
        .set("FileDescription", "AudCap meeting recorder")
        .set("ProductName", "AudCap")
        .set_manifest_file("assets/audcap.manifest");
    res.compile().expect("compile Windows resources");
}

/// Copies `../../frontend/dist` to `$OUT_DIR/frontend` (or writes a placeholder).
fn stage_frontend() {
    let dist = Path::new("../../frontend/dist");
    println!("cargo:rerun-if-changed=../../frontend/dist");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("frontend");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("create frontend staging dir");
    if dist.join("index.html").is_file() {
        copy_dir(dist, &out).expect("copy frontend/dist");
    } else {
        println!("cargo:warning=frontend/dist missing; embedding a placeholder (run build.ps1 to build the app)");
        std::fs::write(
            out.join("index.html"),
            "<!doctype html><title>AudCap</title><p>Frontend not built. Run build.ps1.</p>",
        )
        .expect("write placeholder");
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// ICO with one 32-bit BMP image per size.
fn write_ico(path: &Path, sizes: &[u32]) -> std::io::Result<()> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_entry(s, &icon::render(s, [220, 40, 40]))).collect();
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
