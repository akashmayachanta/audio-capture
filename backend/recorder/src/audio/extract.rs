//! One channel of a recording as 16 kHz mono 16-bit WAV, for speech-to-text
//! services that take plain audio files (daily-driver's "Speech API"
//! transcription). A time window keeps each piece small (OpenAI's limit is
//! 25 MB); packets before the window are only counted, not decoded.

use std::io::Read;

use anyhow::{bail, Context, Result};
use audiopus_sys as ffi;

pub const OUT_RATE: u32 = 16_000;
const IN_RATE: i32 = 48_000;
const DECIMATE: usize = (IN_RATE as u32 / OUT_RATE) as usize;
/// Decoding starts this far before the window so the decoder has settled (RFC 7845 asks for 80 ms).
const PREROLL: u64 = 3840;
/// Longest Opus packet: 120 ms at 48 kHz.
const MAX_FRAME: usize = 5760;

/// What `OpusHead` says about the streams.
#[derive(Clone, Debug, PartialEq)]
pub struct Head {
    pub channels: usize,
    pub pre_skip: u64,
    pub streams: i32,
    pub coupled: i32,
    pub mapping: Vec<u8>,
}

pub fn parse_head(p: &[u8]) -> Result<Head> {
    if p.len() < 19 || &p[..8] != b"OpusHead" {
        bail!("not an Ogg Opus file");
    }
    let channels = p[9] as usize;
    let pre_skip = u16::from_le_bytes([p[10], p[11]]) as u64;
    if channels == 0 {
        bail!("no channels");
    }
    if p[18] == 0 {
        if channels > 2 {
            bail!("mapping family 0 with {channels} channels");
        }
        return Ok(Head { channels, pre_skip, streams: 1, coupled: (channels == 2) as i32, mapping: (0..channels as u8).collect() });
    }
    if p.len() < 21 + channels {
        bail!("short OpusHead");
    }
    Ok(Head { channels, pre_skip, streams: p[19] as i32, coupled: p[20] as i32, mapping: p[21..21 + channels].to_vec() })
}

/// One decoded channel's piece.
pub struct Piece {
    /// 16 kHz mono samples of `[start, start + secs)` (shorter at the end of the file).
    pub samples: Vec<i16>,
    /// Length of the whole recording.
    pub total_secs: f64,
    pub channels: usize,
}

/// Channel `ch` of the Ogg Opus stream in `r`, from `start_secs` for `secs`.
pub fn extract(r: impl Read + std::io::Seek, ch: usize, start_secs: f64, secs: f64) -> Result<Piece> {
    let mut packets = ogg::PacketReader::new(r);
    let head = parse_head(&packets.read_packet().context("read OpusHead")?.context("empty file")?.data)?;
    if ch >= head.channels {
        bail!("channel {ch} out of range (the file has {})", head.channels);
    }
    packets.read_packet().context("read OpusTags")?.context("no OpusTags")?;

    let win_start = (start_secs.max(0.0) * IN_RATE as f64) as u64;
    let win_end = win_start + (secs.max(0.0) * IN_RATE as f64) as u64;
    let mut decoder: Option<Decoder> = None;
    let mut pcm = vec![0f32; MAX_FRAME * head.channels];
    let mut wide = Vec::new(); // 48 kHz samples of the window
    let mut pos: u64 = 0; // samples so far, pre-skip included
    while let Some(packet) = packets.read_packet().context("read packet")? {
        let data = &packet.data;
        let n = unsafe { ffi::opus_packet_get_nb_samples(data.as_ptr(), data.len() as i32, IN_RATE) };
        if n <= 0 {
            continue; // damaged packet: skip it like a decoder's packet loss would
        }
        let n = n as u64;
        // Output-time span of this packet (pre-skip removed); may start negative.
        let (t0, t1) = (pos as i64 - head.pre_skip as i64, (pos + n) as i64 - head.pre_skip as i64);
        pos += n;
        if t1 <= win_start.saturating_sub(PREROLL) as i64 || t0 >= win_end as i64 {
            if t0 >= win_end as i64 {
                decoder = None; // past the window: just count the rest for the total
            }
            continue;
        }
        let dec = match &mut decoder {
            Some(d) => d,
            None => decoder.insert(Decoder::new(&head)?),
        };
        let got = dec.decode(data, &mut pcm)?;
        for i in 0..got {
            let t = t0 + i as i64;
            if t >= win_start as i64 && t < win_end as i64 {
                wide.push(pcm[i * head.channels + ch]);
            }
        }
    }
    let total = pos.saturating_sub(head.pre_skip) as f64 / IN_RATE as f64;
    Ok(Piece { samples: decimate(&wide), total_secs: total, channels: head.channels })
}

struct Decoder(*mut ffi::OpusMSDecoder, usize);

impl Decoder {
    fn new(h: &Head) -> Result<Self> {
        let mut err = 0;
        let st = unsafe { ffi::opus_multistream_decoder_create(IN_RATE, h.channels as i32, h.streams, h.coupled, h.mapping.as_ptr(), &mut err) };
        if st.is_null() || err != 0 {
            bail!("opus decoder: error {err}");
        }
        Ok(Self(st, h.channels))
    }

    /// Samples per channel decoded into `pcm` (interleaved).
    fn decode(&mut self, data: &[u8], pcm: &mut [f32]) -> Result<usize> {
        let n = unsafe { ffi::opus_multistream_decode_float(self.0, data.as_ptr(), data.len() as i32, pcm.as_mut_ptr(), (pcm.len() / self.1) as i32, 0) };
        if n < 0 {
            bail!("opus decode: error {n}");
        }
        Ok(n as usize)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_multistream_decoder_destroy(self.0) }
    }
}

/// Low-pass windowed-sinc FIR (cutoff 7 kHz) then every 3rd sample: 48 kHz -> 16 kHz.
pub fn decimate(x: &[f32]) -> Vec<i16> {
    const TAPS: usize = 63;
    let fc = 7000.0 / IN_RATE as f64;
    let mid = (TAPS / 2) as isize;
    let mut h: Vec<f32> = (0..TAPS as isize)
        .map(|i| {
            let k = (i - mid) as f64;
            let sinc = if k == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * k).sin() / (std::f64::consts::PI * k) };
            let w = 0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / (TAPS - 1) as f64).cos();
            (sinc * w) as f32
        })
        .collect();
    let sum: f32 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= sum);
    (0..x.len() / DECIMATE)
        .map(|k| {
            let c = (k * DECIMATE) as isize;
            let mut acc = 0f32;
            for (j, tap) in h.iter().enumerate() {
                let idx = c + j as isize - mid;
                if idx >= 0 && (idx as usize) < x.len() {
                    acc += tap * x[idx as usize];
                }
            }
            (acc.clamp(-1.0, 1.0) * 32767.0).round() as i16
        })
        .collect()
}

/// A 16-bit mono PCM WAV file.
pub fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::opus_ogg::OggOpusWriter;
    use std::io::Cursor;

    /// 2 s stereo file: left = 440 Hz tone, right = silence.
    fn stereo_file(secs: f64) -> Vec<u8> {
        let mut w = OggOpusWriter::new(Vec::new(), 2, 32_000, &[]).unwrap();
        let frames = (secs * 50.0) as usize;
        let mut n = 0usize;
        for _ in 0..frames {
            let mut pcm = vec![0f32; 960 * 2];
            for i in 0..960 {
                pcm[i * 2] = 0.5 * (2.0 * std::f32::consts::PI * 440.0 * (n + i) as f32 / 48_000.0).sin();
            }
            n += 960;
            w.write_frame(&pcm).unwrap();
        }
        w.finish().unwrap()
    }

    fn rms(s: &[i16]) -> f64 {
        (s.iter().map(|&v| (v as f64 / 32768.0).powi(2)).sum::<f64>() / s.len().max(1) as f64).sqrt()
    }

    #[test]
    fn extracts_one_channel_at_16k() {
        let file = stereo_file(2.0);
        let left = extract(Cursor::new(&file), 0, 0.0, 10.0).unwrap();
        assert_eq!(left.channels, 2);
        assert!((left.total_secs - 2.0).abs() < 0.05, "{}", left.total_secs);
        assert!((left.samples.len() as f64 - 32_000.0).abs() < 400.0, "{}", left.samples.len());
        assert!(rms(&left.samples[1600..]) > 0.2, "the tone is there");
        let right = extract(Cursor::new(&file), 1, 0.0, 10.0).unwrap();
        assert!(rms(&right.samples) < 0.01, "the silent channel stays silent");
    }

    #[test]
    fn windows_cut_the_file() {
        let file = stereo_file(2.0);
        let a = extract(Cursor::new(&file), 0, 0.5, 1.0).unwrap();
        assert_eq!(a.samples.len(), 16_000);
        assert!(rms(&a.samples[800..]) > 0.2);
        let tail = extract(Cursor::new(&file), 0, 1.5, 10.0).unwrap();
        assert!((tail.samples.len() as f64 - 8_000.0).abs() < 400.0, "{}", tail.samples.len());
        let past = extract(Cursor::new(&file), 0, 5.0, 10.0).unwrap();
        assert!(past.samples.is_empty());
        assert!((past.total_secs - 2.0).abs() < 0.05);
    }

    #[test]
    fn bad_input_is_an_error() {
        let file = stereo_file(0.2);
        assert!(extract(Cursor::new(&file), 2, 0.0, 1.0).is_err());
        assert!(extract(Cursor::new(b"nope".to_vec()), 0, 0.0, 1.0).is_err());
        assert!(parse_head(b"OpusHead").is_err());
    }

    #[test]
    fn mono_head_uses_one_stream() {
        let mut h = b"OpusHead".to_vec();
        h.extend_from_slice(&[1, 1, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
        let head = parse_head(&h).unwrap();
        assert_eq!((head.channels, head.pre_skip, head.streams, head.coupled), (1, 312, 1, 0));
    }

    #[test]
    fn wav_header_describes_the_data() {
        let w = wav(&[1, -1], 16_000);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 4);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        assert_eq!(&w[36..40], b"data");
        assert_eq!(&w[44..], &[1, 0, 0xff, 0xff]);
    }

    #[test]
    fn decimate_keeps_low_tones_and_drops_high_ones() {
        let tone = |f: f32| (0..48_000).map(|i| 0.5 * (2.0 * std::f32::consts::PI * f * i as f32 / 48_000.0).sin()).collect::<Vec<_>>();
        assert!(rms(&decimate(&tone(1000.0))[100..15_900]) > 0.3);
        assert!(rms(&decimate(&tone(12_000.0))[100..15_900]) < 0.02, "would alias to 4 kHz");
    }
}
