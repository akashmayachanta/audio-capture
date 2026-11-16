//! Turning raw device buffers into mono f32 at 48 kHz.
//!
//! Normally WASAPI's AUTOCONVERTPCM hands us exactly that and none of this does any
//! work. When a driver refuses autoconvert we capture the mix format instead and
//! convert here: decode → downmix → (rubato) resample.

use anyhow::Result;

use super::SAMPLE_RATE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    I16,
    I32,
}

/// Decodes interleaved little-endian samples and averages the channels.
pub fn decode_downmix(bytes: &[u8], format: SampleFormat, channels: usize, out: &mut Vec<f32>) {
    let width = match format {
        SampleFormat::F32 | SampleFormat::I32 => 4,
        SampleFormat::I16 => 2,
    };
    let scale = 1.0 / channels as f32;
    for frame in bytes.chunks_exact(width * channels) {
        let sum: f32 = frame
            .chunks_exact(width)
            .map(|b| match format {
                SampleFormat::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                SampleFormat::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0,
                SampleFormat::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
            })
            .sum();
        out.push(sum * scale);
    }
}

/// How many samples of silence to insert when a source has stalled.
///
/// `expected` is how many samples wall-clock time says should have arrived,
/// `delivered` how many did. A source that is more than `GAP` behind (loopback
/// with nothing playing, a device being reopened) is topped up to within `LEAD`
/// of real time, so the stream stays continuous without racing ahead of data
/// that is merely late.
pub fn silence_needed(expected: u64, delivered: u64) -> usize {
    if expected > delivered + GAP { (expected - delivered - LEAD) as usize } else { 0 }
}

/// 100 ms.
pub const GAP: u64 = SAMPLE_RATE as u64 / 10;
/// 20 ms.
pub const LEAD: u64 = SAMPLE_RATE as u64 / 50;

pub struct Converter {
    format: SampleFormat,
    channels: usize,
    resampler: Option<Resampler>,
    mono: Vec<f32>,
}

impl Converter {
    pub fn new(format: SampleFormat, channels: usize, rate: u32) -> Result<Self> {
        let resampler = if rate == SAMPLE_RATE { None } else { Some(Resampler::new(rate)?) };
        Ok(Self { format, channels, resampler, mono: Vec::new() })
    }

    /// Appends converted 48 kHz mono samples to `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<f32>) -> Result<()> {
        match &mut self.resampler {
            None => decode_downmix(bytes, self.format, self.channels, out),
            Some(r) => {
                self.mono.clear();
                decode_downmix(bytes, self.format, self.channels, &mut self.mono);
                r.push(&self.mono, out)?;
            }
        }
        Ok(())
    }
}

/// Mono fixed-input FFT resampler to 48 kHz, fed in arbitrary-sized pieces.
struct Resampler {
    inner: rubato::Fft<f32>,
    pending: Vec<f32>,
    out: Vec<f32>,
}

impl Resampler {
    fn new(rate: u32) -> Result<Self> {
        // 10 ms chunks.
        let chunk = (rate / 100) as usize;
        let inner = rubato::Fft::<f32>::new(rate as usize, SAMPLE_RATE as usize, chunk, 1, rubato::FixedSync::Input)?;
        let out = vec![0.0; rubato::Resampler::output_frames_max(&inner)];
        Ok(Self { inner, pending: Vec::new(), out })
    }

    fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        use audioadapter_buffers::direct::InterleavedSlice;
        use rubato::Resampler as _;
        self.pending.extend_from_slice(input);
        let mut used = 0;
        loop {
            let need = self.inner.input_frames_next();
            if self.pending.len() - used < need {
                break;
            }
            let inp = InterleavedSlice::new(&self.pending[used..used + need], 1, need)?;
            let cap = self.out.len();
            let mut outp = InterleavedSlice::new_mut(&mut self.out, 1, cap)?;
            let (n_in, n_out) = self.inner.process_into_buffer(&inp, &mut outp, None)?;
            used += n_in;
            out.extend_from_slice(&self.out[..n_out]);
        }
        self.pending.drain(..used);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_stereo_is_averaged_to_mono() {
        let mut bytes = Vec::new();
        for s in [0.5f32, 0.25, -1.0, 1.0] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        decode_downmix(&bytes, SampleFormat::F32, 2, &mut out);
        assert_eq!(out, vec![0.375, 0.0]);
    }

    #[test]
    fn i16_is_scaled_to_unit_range() {
        let mut bytes = Vec::new();
        for s in [i16::MIN, 0, 16384] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        decode_downmix(&bytes, SampleFormat::I16, 1, &mut out);
        assert_eq!(out, vec![-1.0, 0.0, 0.5]);
    }

    #[test]
    fn i32_is_scaled_to_unit_range() {
        let mut bytes = Vec::new();
        for s in [i32::MIN, 1 << 30] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut out = Vec::new();
        decode_downmix(&bytes, SampleFormat::I32, 1, &mut out);
        assert_eq!(out, vec![-1.0, 0.5]);
    }

    #[test]
    fn no_silence_when_on_time_or_slightly_late() {
        assert_eq!(silence_needed(48_000, 48_000), 0);
        assert_eq!(silence_needed(48_000 + GAP, 48_000), 0);
        assert_eq!(silence_needed(10, 1_000), 0, "ahead of the clock");
    }

    #[test]
    fn stalled_source_is_topped_up_to_within_lead() {
        let expected = 48_000 * 3;
        let delivered = 48_000;
        assert_eq!(silence_needed(expected, delivered) as u64, expected - delivered - LEAD);
    }

    #[test]
    fn converter_resamples_44k1_to_48k() {
        let mut c = Converter::new(SampleFormat::F32, 1, 44_100).unwrap();
        let mut bytes = Vec::new();
        for _ in 0..44_100 {
            bytes.extend_from_slice(&0.1f32.to_le_bytes());
        }
        let mut out = Vec::new();
        c.push(&bytes, &mut out).unwrap();
        // One second in, ~one second out (minus resampler latency).
        assert!(out.len() > 46_000 && out.len() <= 48_000, "got {}", out.len());
    }
}
