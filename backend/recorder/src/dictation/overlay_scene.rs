//! What the dictation overlay looks like, as pure pixel maths: a small round
//! badge with a microphone in it. Listening: a halo and ripples that follow
//! the voice. Starting the service / transcribing: a light running round the
//! rim (amber / indigo). Error: a red flash.
//! Software-rendered premultiplied BGRA (`0xAARRGGBB`), no dependencies, like `icon.rs`.

use std::f32::consts::TAU;

/// Default size in pixels at 100% display scale; `render` works at any size with the same proportions.
pub const W: usize = 96;
pub const H: usize = 96;

/// Radius of the disc, in design pixels (the window is `H` = 96 of them tall).
const DISC: f32 = 26.0;
/// How far the halo reaches beyond the disc at full voice level.
const HALO: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The mic is live; the halo follows the voice.
    Listening,
    /// The speech service is starting or its model is loading.
    Starting,
    /// Recording finished; the text is on its way.
    Transcribing,
    Error,
}

#[derive(Clone, Debug)]
pub struct Scene {
    pub phase: Phase,
    /// Smoothed mic level, 0..1.
    pub level: f32,
    /// Seconds since the overlay was shown; drives the animation.
    pub t: f32,
    pub dark: bool,
    /// 0..1 multiplier for fading in and out.
    pub fade: f32,
}

type Rgb = [f32; 3];
/// Premultiplied: r, g, b, a.
type Px = [f32; 4];

/// The app's colour tokens (`frontend/src/app.css`): me, accent, warn, danger.
fn phase_colour(phase: Phase, dark: bool) -> Rgb {
    let hex = |h: u32| [(h >> 16 & 0xFF) as f32 / 255.0, (h >> 8 & 0xFF) as f32 / 255.0, (h & 0xFF) as f32 / 255.0];
    match (phase, dark) {
        (Phase::Listening, true) => hex(0x3ccbb6),
        (Phase::Listening, false) => hex(0x0f8f7e),
        (Phase::Transcribing, true) => hex(0x8b95f7),
        (Phase::Transcribing, false) => hex(0x4f5bd5),
        (Phase::Starting, true) => hex(0xe6b04f),
        (Phase::Starting, false) => hex(0xb7791f),
        (Phase::Error, true) => hex(0xf07171),
        (Phase::Error, false) => hex(0xd23b3b),
    }
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// `src` over `dst`, both premultiplied.
fn over(dst: Px, src: Px) -> Px {
    let k = 1.0 - src[3];
    [src[0] + dst[0] * k, src[1] + dst[1] * k, src[2] + dst[2] * k, src[3] + dst[3] * k]
}

fn px(colour: Rgb, alpha: f32) -> Px {
    [colour[0] * alpha, colour[1] * alpha, colour[2] * alpha, alpha]
}

/// Signed distance from (x, y) to a rounded rectangle centred at (cx, cy); negative inside.
fn sd_round_rect(x: f32, y: f32, cx: f32, cy: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = (x - cx).abs() - (half_w - radius);
    let qy = (y - cy).abs() - (half_h - radius);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius
}

/// Signed distance to the microphone glyph, in design pixels, for a point
/// (x, y) relative to the centre of the disc (y grows downwards). The glyph is
/// 20 wide and 28 tall: head, cradle, stem, base.
fn sd_mic(x: f32, y: f32) -> f32 {
    const STROKE: f32 = 1.1; // half the line width
    let head = sd_round_rect(x, y, 0.0, -5.0, 5.0, 9.0, 5.0);
    // The cradle: the lower half of a circle round the head, with round ends.
    let (ax, ay) = (x.abs(), y + 3.0);
    let cradle = if ay >= 0.0 { ((ax * ax + ay * ay).sqrt() - 9.0).abs() } else { ((ax - 9.0).powi(2) + ay * ay).sqrt() } - STROKE;
    let stem = sd_round_rect(x, y, 0.0, 9.5, STROKE, 3.5, STROKE);
    let base = sd_round_rect(x, y, 0.0, 13.0, 5.0, STROKE, STROKE);
    head.min(cradle).min(stem).min(base)
}

/// Brightness of the rim at `angle` (radians): a light with a fading tail
/// running clockwise, `turns` times a second.
fn comet(angle: f32, t: f32, turns: f32) -> f32 {
    let behind = (t * turns - angle / TAU).rem_euclid(1.0); // 0 at the head, 1 a full turn behind
    (1.0 - behind).powi(3)
}

/// Draws `scene` into `out` (`w * h` premultiplied BGRA pixels). A buffer of the
/// wrong size is left untouched.
pub fn render(scene: &Scene, w: usize, h: usize, out: &mut [u32]) {
    if out.len() != w * h || w == 0 || h == 0 {
        return;
    }
    let u = h as f32 / H as f32; // one design pixel
    let level = if scene.level.is_finite() { scene.level.clamp(0.0, 1.0) } else { 0.0 };
    let fade = scene.fade.clamp(0.0, 1.0);
    let t = scene.t;
    let colour = phase_colour(scene.phase, scene.dark);
    let listening = scene.phase == Phase::Listening;

    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    // The halo outside the disc: a soft glow, wider and stronger with the voice.
    let (glow_reach, glow) = match scene.phase {
        Phase::Listening => (8.0 + 2.0 * (t * 2.0).sin() + (HALO - 10.0) * level, 0.25 + 0.45 * level),
        Phase::Error => (10.0, 0.35 + 0.15 * (t * 8.0).sin()),
        _ => (8.0, 0.25),
    };
    let ripple_gain = 0.10 + 0.45 * level;
    let body = if scene.dark { px([24.0 / 255.0, 26.0 / 255.0, 33.0 / 255.0], 0.92) } else { px([252.0 / 255.0, 252.0 / 255.0, 253.0 / 255.0], 0.94) };
    let rim_w = 1.5;
    // The microphone swells a little with the voice.
    let mic_scale = if listening { 1.0 + 0.10 * level } else { 1.0 };

    for y in 0..h {
        for x in 0..w {
            // Design pixels from the centre of the disc.
            let (dx, dy) = ((x as f32 + 0.5 - cx) / u, (y as f32 + 0.5 - cy) / u);
            let sd = (dx * dx + dy * dy).sqrt() - DISC;
            let mut p: Px = [0.0; 4];
            if sd > 0.0 && sd < HALO {
                if sd < glow_reach {
                    p = over(p, px(colour, (1.0 - sd / glow_reach).powi(2) * glow));
                }
                if listening {
                    // Two rings travelling outwards, fading as they go.
                    for k in 0..2 {
                        let travel = (t * 0.8 + k as f32 * 0.5).rem_euclid(1.0);
                        let band = (1.0 - (sd - travel * HALO).abs() / 1.5).max(0.0);
                        let edge = 1.0 - sd / HALO; // nothing at the very end of the reach
                        p = over(p, px(colour, band * (1.0 - travel) * edge * ripple_gain));
                    }
                }
            }
            let cover = (0.5 - sd * u).clamp(0.0, 1.0);
            if cover > 0.0 {
                p = over(p, [body[0] * cover, body[1] * cover, body[2] * cover, body[3] * cover]);
                let rim = ((rim_w * 0.5 - (sd + rim_w * 0.5).abs()) * u + 0.5).clamp(0.0, 1.0);
                if rim > 0.0 {
                    let alpha = match scene.phase {
                        Phase::Listening => 0.55 + 0.35 * level,
                        Phase::Starting => 0.12 + 0.88 * comet(dy.atan2(dx), t, 0.9),
                        Phase::Transcribing => 0.12 + 0.88 * comet(dy.atan2(dx), t, 1.6),
                        Phase::Error => 0.65 + 0.3 * (t * 8.0).sin(),
                    };
                    p = over(p, px(colour, rim * alpha.clamp(0.0, 1.0) * cover));
                }
                let mic = (0.5 - sd_mic(dx / mic_scale, dy / mic_scale) * mic_scale * u).clamp(0.0, 1.0);
                if mic > 0.0 {
                    let (c, alpha) = match scene.phase {
                        Phase::Listening | Phase::Error => (colour, 0.95),
                        Phase::Starting => (colour, 0.65 + 0.25 * (t * 3.0).sin()),
                        // A highlight sweeping up the microphone.
                        Phase::Transcribing => (mix(colour, [1.0, 1.0, 1.0], 0.45 * (0.5 + 0.5 * (t * 6.0 + dy * 0.25).sin())), 0.95),
                    };
                    p = over(p, px(c, mic * alpha));
                }
            }
            let to_u8 = |v: f32| (v * fade * 255.0).round().clamp(0.0, 255.0) as u32;
            out[y * w + x] = to_u8(p[3]) << 24 | to_u8(p[0]) << 16 | to_u8(p[1]) << 8 | to_u8(p[2]);
        }
    }
}

/// Smooths the raw mic level for display: quick to rise, slow to fall.
#[derive(Default)]
pub struct Smoother {
    value: f32,
}

impl Smoother {
    /// `dt` is the seconds since the last call.
    pub fn push(&mut self, level: f32, dt: f32) -> f32 {
        let target = if level.is_finite() { level.clamp(0.0, 1.0) } else { 0.0 };
        let dt = if dt.is_finite() { dt.clamp(0.0, 0.25) } else { 0.0 };
        let tau = if target > self.value { 0.015 } else { 0.25 };
        self.value += (target - self.value) * (1.0 - (-dt / tau).exp());
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(scene: &Scene) -> Vec<u32> {
        let mut out = vec![0u32; W * H];
        render(scene, W, H, &mut out);
        out
    }

    fn listening(level: f32, t: f32) -> Scene {
        Scene { phase: Phase::Listening, level, t, dark: true, fade: 1.0 }
    }

    fn alpha(p: u32) -> u32 {
        p >> 24
    }

    fn channels(p: u32) -> (u32, u32, u32) {
        ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF)
    }

    fn at(buf: &[u32], x: usize, y: usize) -> u32 {
        buf[y * W + x]
    }

    #[test]
    fn corners_are_transparent_and_the_centre_is_not() {
        let f = frame(&listening(0.5, 0.3));
        for (x, y) in [(0, 0), (W - 1, 0), (0, H - 1), (W - 1, H - 1)] {
            assert_eq!(at(&f, x, y), 0, "corner {x},{y}");
        }
        assert!(alpha(at(&f, W / 2, H / 2)) > 100);
    }

    #[test]
    fn every_pixel_is_valid_premultiplied() {
        for phase in [Phase::Listening, Phase::Starting, Phase::Transcribing, Phase::Error] {
            for dark in [true, false] {
                let f = frame(&Scene { phase, level: 0.8, t: 1.7, dark, fade: 0.8 });
                for (i, p) in f.iter().enumerate() {
                    let (r, g, b) = channels(*p);
                    assert!(r <= alpha(*p) && g <= alpha(*p) && b <= alpha(*p), "{phase:?} pixel {i}: {p:#010x}");
                }
            }
        }
    }

    /// Sum of the brightness of all pixels.
    fn ink(buf: &[u32]) -> u64 {
        buf.iter().map(|p| {
            let (r, g, b) = channels(*p);
            (r + g + b) as u64
        }).sum()
    }

    #[test]
    fn a_louder_voice_makes_the_halo_bigger() {
        let quiet = frame(&listening(0.0, 0.4));
        let loud = frame(&listening(1.0, 0.4));
        assert_ne!(quiet, loud);
        assert!(ink(&loud) > ink(&quiet), "{} vs {}", ink(&loud), ink(&quiet));
        // Outside the disc: only the halo is there, and it reaches further when loud.
        let (x, y) = (W / 2 + 38, H / 2);
        assert!(alpha(at(&loud, x, y)) > alpha(at(&quiet, x, y)) + 10, "{:#x} vs {:#x}", at(&loud, x, y), at(&quiet, x, y));
    }

    #[test]
    fn it_is_a_small_round_badge() {
        assert_eq!(W, H);
        assert!(W <= 96);
        let f = frame(&listening(1.0, 0.3));
        // Nothing reaches the middle of an edge, even at full level.
        for (x, y) in [(0, H / 2), (W - 1, H / 2), (W / 2, 0), (W / 2, H - 1)] {
            assert_eq!(at(&f, x, y), 0, "edge {x},{y}");
        }
        // The disc is as tall as it is wide.
        let solid = |x: usize, y: usize| alpha(at(&f, x, y)) > 200;
        assert!(solid(W / 2 - 22, H / 2) && solid(W / 2 + 22, H / 2) && solid(W / 2, H / 2 - 22) && solid(W / 2, H / 2 + 22));
    }

    #[test]
    fn a_microphone_sits_in_the_middle() {
        let f = frame(&Scene { phase: Phase::Listening, level: 0.0, t: 0.0, dark: true, fade: 1.0 });
        let bright = |x: usize, y: usize| {
            let (r, g, b) = channels(at(&f, x, y));
            r + g + b
        };
        let (cx, cy) = (W / 2, H / 2);
        // Head, stem and base are drawn in the phase colour on the dark disc.
        for (x, y) in [(cx, cy - 6), (cx, cy + 10), (cx - 3, cy + 13), (cx + 3, cy + 13)] {
            assert!(bright(x, y) > 300, "mic at {x},{y}: {:#x}", at(&f, x, y));
        }
        // Beside the head and under the base it is just the disc.
        for (x, y) in [(cx - 16, cy - 6), (cx + 16, cy - 6), (cx, cy + 20)] {
            assert!(bright(x, y) < 150, "disc at {x},{y}: {:#x}", at(&f, x, y));
        }
        let (r, g, b) = channels(at(&f, cx, cy - 6));
        assert!(g > r && b > r, "the mic is teal while listening: {r} {g} {b}");
    }

    #[test]
    fn starting_spins_over_time() {
        let a = Scene { phase: Phase::Starting, level: 0.0, t: 0.0, dark: true, fade: 1.0 };
        assert_ne!(frame(&a), frame(&Scene { t: 0.3, ..a.clone() }));
    }

    #[test]
    fn a_quiet_mic_still_breathes() {
        assert_ne!(frame(&listening(0.0, 0.0)), frame(&listening(0.0, 0.7)));
    }

    #[test]
    fn transcribing_shimmers_over_time() {
        let a = Scene { phase: Phase::Transcribing, level: 0.0, t: 0.0, dark: true, fade: 1.0 };
        let b = Scene { t: 0.5, ..a.clone() };
        assert_ne!(frame(&a), frame(&b));
    }

    fn average_colour(buf: &[u32]) -> (f64, f64, f64) {
        // The head of the microphone.
        let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0.0);
        for y in H / 2 - 12..H / 2 + 2 {
            for x in W / 2 - 4..W / 2 + 4 {
                let (pr, pg, pb) = channels(at(buf, x, y));
                r += pr as f64;
                g += pg as f64;
                b += pb as f64;
                n += 1.0;
            }
        }
        (r / n, g / n, b / n)
    }

    #[test]
    fn each_phase_has_its_colour() {
        let phase = |p| average_colour(&frame(&Scene { phase: p, level: 0.7, t: 0.9, dark: true, fade: 1.0 }));
        let (r, _, b) = phase(Phase::Error);
        assert!(r > b + 20.0, "error is red: {r} {b}");
        let (r, g, b) = phase(Phase::Starting);
        assert!(r > b + 10.0 && g > b + 5.0, "starting is amber: {r} {g} {b}");
        let (r, g, b) = phase(Phase::Listening);
        assert!(g > r && b > r, "listening is the teal 'me' colour: {r} {g} {b}");
        let (r, g, b) = phase(Phase::Transcribing);
        assert!(b > g, "transcribing is the indigo accent: {r} {g} {b}");
    }

    #[test]
    fn light_and_dark_differ() {
        let dark = frame(&Scene { dark: true, ..listening(0.5, 0.0) });
        let light = frame(&Scene { dark: false, ..listening(0.5, 0.0) });
        assert!(ink(&light) > ink(&dark), "the light disc is brighter");
    }

    #[test]
    fn fade_scales_everything() {
        let full = frame(&Scene { fade: 1.0, ..listening(0.5, 0.2) });
        assert!(frame(&Scene { fade: 0.0, ..listening(0.5, 0.2) }).iter().all(|p| *p == 0));
        let half = frame(&Scene { fade: 0.5, ..listening(0.5, 0.2) });
        let (a_full, a_half) = (alpha(at(&full, W / 2, H / 2)) as i32, alpha(at(&half, W / 2, H / 2)) as i32);
        assert!((a_half * 2 - a_full).abs() <= 2, "{a_half} vs {a_full}");
    }

    #[test]
    fn it_renders_at_any_size() {
        let (w, h) = (W * 2, H * 2);
        let mut out = vec![0u32; w * h];
        render(&listening(0.6, 0.1), w, h, &mut out);
        assert_eq!(out[0], 0);
        assert!(alpha(out[(h / 2) * w + w / 2]) > 100);
        // A buffer that does not match the size is left alone rather than overrun.
        let mut small = vec![7u32; 10];
        render(&listening(0.6, 0.1), w, h, &mut small);
        assert!(small.iter().all(|p| *p == 7));
    }

    #[test]
    fn the_smoother_attacks_fast_and_releases_slowly() {
        let mut s = Smoother::default();
        let up = s.push(1.0, 0.016);
        assert!(up >= 0.6, "{up}");
        let down = s.push(0.0, 0.016);
        assert!(down >= 0.5 && down < up, "{down}");
        for _ in 0..120 {
            s.push(0.0, 0.016);
        }
        assert!(s.push(0.0, 0.016) < 0.05, "it does settle");
    }

    #[test]
    fn the_smoother_ignores_nonsense() {
        let mut s = Smoother::default();
        assert_eq!(s.push(f32::NAN, 0.016), 0.0);
        assert!(s.push(7.0, 0.016) <= 1.0);
        assert!(s.push(-3.0, 0.016) >= 0.0);
    }
}
