//! The MeetRec mark: a rounded square in a colour with a white sound wave
//! (vertical bars). Used for the tray (state colours) and, via `#[path]` from
//! build.rs, for the exe icon, so it has no dependencies.

/// Straight-alpha RGBA, `size` x `size`, anti-aliased by 4x4 supersampling.
pub fn render(size: u32, rgb: [u8; 3]) -> Vec<u8> {
    // Fewer, wider bars when small, so they stay distinct at 16 px.
    let heights: &[f32] = if size >= 24 { &[0.26, 0.46, 0.62, 0.46, 0.26] } else { &[0.36, 0.62, 0.36] };
    let n = heights.len() as f32;
    let w = 0.56 / (2.0 * n - 1.0); // bar width = gap width
    let first = 0.5 - 0.28 + w / 2.0;
    let s = size as f32;
    const SS: u32 = 4;
    let mut px = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (mut bg, mut bar) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / s;
                    let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / s;
                    if !in_rounded_rect(u, v, 0.5, 0.5, 0.47, 0.47, 0.24) {
                        continue;
                    }
                    bg += 1;
                    let i = ((u - first) / (2.0 * w)).round();
                    if (0.0..n).contains(&i) {
                        let cx = first + i * 2.0 * w;
                        let h = heights[i as usize] / 2.0;
                        if in_rounded_rect(u, v, cx, 0.5, w / 2.0, h, w / 2.0) {
                            bar += 1;
                        }
                    }
                }
            }
            let a = bg as f32 / (SS * SS) as f32;
            let white = if bg == 0 { 0.0 } else { bar as f32 / bg as f32 };
            let mix = |c: u8| (c as f32 + (255.0 - c as f32) * white).round() as u8;
            px.extend_from_slice(&[mix(rgb[0]), mix(rgb[1]), mix(rgb[2]), (a * 255.0).round() as u8]);
        }
    }
    px
}

/// Point (u, v) inside the rectangle centred at (cx, cy) with half sizes
/// (hw, hh) and corner radius r.
fn in_rounded_rect(u: f32, v: f32, cx: f32, cy: f32, hw: f32, hh: f32, r: f32) -> bool {
    let dx = ((u - cx).abs() - (hw - r)).max(0.0);
    let dy = ((v - cy).abs() - (hh - r)).max(0.0);
    (u - cx).abs() <= hw && (v - cy).abs() <= hh && dx * dx + dy * dy <= r * r
}
