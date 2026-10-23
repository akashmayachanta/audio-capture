//! Turns a recording into what the transcription helper reads: one 16 kHz mono
//! 16-bit WAV per channel (`me.wav`, and `others.wav` for stereo recordings).
//!
//! Decoding streams packet by packet into the WAV files, so an hour-long
//! stereo recording never has to fit in memory.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

use crate::audio::SAMPLE_RATE;
use crate::playback::reader::OpusFileReader;

/// What the model expects.
pub const TARGET_RATE: u32 = 16_000;

/// Input frames per resampler call (10 ms at 48 kHz).
const CHUNK: usize = 480;

pub struct Prepared {
    pub me: PathBuf,
    /// `None` for mono recordings.
    pub others: Option<PathBuf>,
    pub duration_secs: f64,
}

/// Decodes `opus` into WAVs in `dir` (created if needed). `on_progress` gets 0..=1.
pub fn prepare(opus: &Path, dir: &Path, mut on_progress: impl FnMut(f32)) -> Result<Prepared> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let mut reader = OpusFileReader::open(opus)?;
    let channels = reader.channels();
    let total = reader.duration_secs().max(0.001);
    let names = ["me.wav", "others.wav"];
    let mut lanes = (0..channels)
        .map(|c| Lane::create(&dir.join(names[c])))
        .collect::<Result<Vec<_>>>()?;

    let mut last_report = 0.0;
    while let Some(chunk) = reader.next_chunk() {
        for (c, lane) in lanes.iter_mut().enumerate() {
            lane.scratch.clear();
            lane.scratch.extend(chunk.iter().skip(c).step_by(channels));
            lane.push()?;
        }
        let progress = (reader.position_secs() / total).min(1.0) as f32;
        if progress - last_report >= 0.01 {
            last_report = progress;
            on_progress(progress);
        }
    }
    let samples = lanes.first().map_or(0, |l| l.in_samples);
    for lane in lanes {
        lane.finish()?;
    }
    on_progress(1.0);
    Ok(Prepared {
        me: dir.join(names[0]),
        others: (channels == 2).then(|| dir.join(names[1])),
        duration_secs: samples as f64 / SAMPLE_RATE as f64,
    })
}

/// One channel on its way to a WAV file.
struct Lane {
    /// De-interleaved 48 kHz samples for the current packet.
    scratch: Vec<f32>,
    resampler: Down16k,
    out: Vec<f32>,
    wav: WavWriter,
    in_samples: u64,
}

impl Lane {
    fn create(path: &Path) -> Result<Self> {
        Ok(Self {
            scratch: Vec::new(),
            resampler: Down16k::new()?,
            out: Vec::new(),
            wav: WavWriter::create(path)?,
            in_samples: 0,
        })
    }

    fn push(&mut self) -> Result<()> {
        self.in_samples += self.scratch.len() as u64;
        self.out.clear();
        self.resampler.push(&self.scratch, &mut self.out)?;
        self.wav.write(&self.out)
    }

    fn finish(mut self) -> Result<()> {
        self.out.clear();
        self.resampler.finish(&mut self.out)?;
        self.wav.write(&self.out)?;
        self.wav.finish()
    }
}

/// Streaming 48 kHz -> 16 kHz mono resampler. The output is exactly
/// `ceil(input / 3)` samples long, with the filter delay trimmed off.
pub struct Down16k {
    inner: Fft<f32>,
    pending: Vec<f32>,
    chunk: Vec<f32>,
    buf: Vec<f32>,
    to_trim: usize,
    fed: usize,
    produced: usize,
}

impl Down16k {
    pub fn new() -> Result<Self> {
        let inner = Fft::<f32>::new(SAMPLE_RATE as usize, TARGET_RATE as usize, CHUNK, 1, FixedSync::Input)?;
        let buf = vec![0.0; inner.output_frames_max()];
        let to_trim = inner.output_delay();
        Ok(Self { inner, pending: Vec::new(), chunk: Vec::new(), buf, to_trim, fed: 0, produced: 0 })
    }

    /// Appends the resampled audio for `input` (any length) to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        self.fed += input.len();
        self.pending.extend_from_slice(input);
        let mut used = 0;
        loop {
            let need = self.inner.input_frames_next();
            if self.pending.len() - used < need {
                break;
            }
            let n_out = self.run(used, None)?;
            used += need;
            self.emit(n_out, out);
        }
        self.pending.drain(..used);
        Ok(())
    }

    /// Flushes the tail. Call once, after the last `push`.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let expected = self.fed.div_ceil((SAMPLE_RATE / TARGET_RATE) as usize);
        if !self.pending.is_empty() {
            let n_out = self.run(0, Some(self.pending.len()))?;
            self.pending.clear();
            self.emit(n_out, out);
        }
        // The delayed tail only comes out when zeros are pumped through.
        while self.produced < expected {
            let before = self.produced;
            let n_out = self.run(0, Some(0))?;
            self.emit(n_out, out);
            if n_out == 0 && self.produced == before {
                break;
            }
        }
        let extra = self.produced.saturating_sub(expected);
        out.truncate(out.len() - extra);
        self.produced -= extra;
        Ok(())
    }

    /// One resampler call over `pending[offset..]` (zero-padded to a whole
    /// chunk); returns the frames written to `buf`.
    fn run(&mut self, offset: usize, partial_len: Option<usize>) -> Result<usize> {
        let need = self.inner.input_frames_next();
        let have = (self.pending.len() - offset).min(need);
        self.chunk.clear();
        self.chunk.extend_from_slice(&self.pending[offset..offset + have]);
        self.chunk.resize(need, 0.0);
        let inp = InterleavedSlice::new(&self.chunk[..], 1, need)?;
        let cap = self.buf.len();
        let mut outp = InterleavedSlice::new_mut(&mut self.buf, 1, cap)?;
        let indexing = Indexing { input_offset: 0, output_offset: 0, partial_len, active_channels_mask: None };
        let (_, n_out) = self.inner.process_into_buffer(&inp, &mut outp, Some(&indexing))?;
        Ok(n_out)
    }

    /// Copies `buf[..n]` to `out`, dropping the start-up delay first.
    fn emit(&mut self, n: usize, out: &mut Vec<f32>) {
        let skip = self.to_trim.min(n);
        self.to_trim -= skip;
        out.extend_from_slice(&self.buf[skip..n]);
        self.produced += n - skip;
    }
}

/// All of `samples` (48 kHz mono) at 16 kHz. For tests and short clips.
pub fn downsample_to_16k(samples: &[f32]) -> Result<Vec<f32>> {
    let mut r = Down16k::new()?;
    let mut out = Vec::new();
    r.push(samples, &mut out)?;
    r.finish(&mut out)?;
    Ok(out)
}

/// 16 kHz mono 16-bit PCM WAV, written as it arrives; the sizes in the header
/// are patched on `finish`.
pub struct WavWriter {
    w: BufWriter<File>,
    samples: u32,
}

impl WavWriter {
    pub fn create(path: &Path) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
        let mut w = BufWriter::new(file);
        w.write_all(&header(0))?;
        Ok(Self { w, samples: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
        }
        self.samples += samples.len() as u32;
        self.w.write_all(&bytes)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.w.seek(SeekFrom::Start(0))?;
        self.w.write_all(&header(self.samples))?;
        self.w.flush()?;
        Ok(())
    }
}

/// The 44-byte canonical header for `samples` 16 kHz mono 16-bit samples.
fn header(samples: u32) -> [u8; 44] {
    let data = samples * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&TARGET_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&(TARGET_RATE * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;
    use crate::encode::opus_ogg::OggOpusWriter;

    const SR: f32 = SAMPLE_RATE as f32;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * freq * 2.0 * std::f32::consts::PI / SR).sin() * amp).collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    /// Samples of a WAV written by `WavWriter`, after checking its header.
    fn read_wav(path: &Path) -> Vec<f32> {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
        let data = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert_eq!(data, bytes.len() - 44, "data size is patched in");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize, bytes.len() - 8);
        bytes[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect()
    }

    fn encode(channels: u8, secs: usize, left: &[f32], right: &[f32]) -> Vec<u8> {
        let mut w = OggOpusWriter::new(Vec::new(), channels, 48_000, &[]).unwrap();
        for f in 0..secs * 50 {
            let mut pcm = Vec::with_capacity(FRAME * channels as usize);
            for n in f * FRAME..(f + 1) * FRAME {
                pcm.push(left[n]);
                if channels == 2 {
                    pcm.push(right[n]);
                }
            }
            w.write_frame(&pcm).unwrap();
        }
        w.finish().unwrap()
    }

    #[test]
    fn wav_header_sizes_are_patched_and_samples_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let mut w = WavWriter::create(&path).unwrap();
        w.write(&[0.0, 0.5, -0.5]).unwrap();
        w.write(&[2.0, -2.0]).unwrap();
        w.finish().unwrap();
        let s = read_wav(&path);
        assert_eq!(s.len(), 5);
        assert!((s[1] - 0.5).abs() < 0.001);
        assert!(s[3] > 0.999 && s[4] < -0.999);
    }

    #[test]
    fn downsampling_is_exactly_a_third_of_the_length_rounded_up() {
        for n in [0usize, 1, 100, 479, 480, 481, 48_000, 48_001, 100_003] {
            let out = downsample_to_16k(&sine(300.0, 0.5, n)).unwrap();
            assert_eq!(out.len(), n.div_ceil(3), "input of {n} samples");
        }
    }

    #[test]
    fn downsampling_keeps_the_tone_and_its_timing() {
        let out = downsample_to_16k(&sine(1000.0, 0.5, 96_000)).unwrap();
        let mid = &out[4000..28_000];
        assert!((rms(mid) - 0.5 / 2f32.sqrt()).abs() < 0.01, "level {}", rms(mid));
        // Delay is trimmed: output sample k is the 1 kHz sine at k / 16 kHz.
        for k in (8000..8100).step_by(7) {
            let want = (k as f32 * 1000.0 * 2.0 * std::f32::consts::PI / 16_000.0).sin() * 0.5;
            assert!((out[k] - want).abs() < 0.02, "k={k}: {} vs {want}", out[k]);
        }
    }

    #[test]
    fn the_piece_size_fed_in_does_not_change_the_result() {
        let input = sine(500.0, 0.4, 50_000);
        let whole = downsample_to_16k(&input).unwrap();
        let mut r = Down16k::new().unwrap();
        let mut pieces = Vec::new();
        for chunk in input.chunks(333) {
            r.push(chunk, &mut pieces).unwrap();
        }
        r.finish(&mut pieces).unwrap();
        assert_eq!(pieces.len(), whole.len());
        let worst = pieces.iter().zip(&whole).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(worst < 1e-4, "worst difference {worst}");
    }

    #[test]
    fn stereo_recording_becomes_two_wavs_with_channels_kept_apart() {
        let dir = tempfile::tempdir().unwrap();
        let opus = dir.path().join("rec.opus");
        std::fs::write(&opus, encode(2, 2, &sine(440.0, 0.5, 96_000), &vec![0.0; 96_000])).unwrap();
        let mut seen = Vec::new();
        let prepared = prepare(&opus, &dir.path().join("job"), |p| seen.push(p)).unwrap();
        assert!((prepared.duration_secs - 2.0).abs() < 0.01, "{}", prepared.duration_secs);
        let me = read_wav(&prepared.me);
        let others = read_wav(prepared.others.as_ref().unwrap());
        assert_eq!(me.len(), 32_000);
        assert_eq!(others.len(), 32_000);
        assert!(rms(&me) > 0.3, "me carries the tone: {}", rms(&me));
        assert!(rms(&others) < 0.001, "others is silent: {}", rms(&others));
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress never goes back");
    }

    #[test]
    fn mono_recording_produces_only_me() {
        let dir = tempfile::tempdir().unwrap();
        let opus = dir.path().join("rec.opus");
        std::fs::write(&opus, encode(1, 1, &sine(440.0, 0.5, 48_000), &[])).unwrap();
        let prepared = prepare(&opus, &dir.path().join("job"), |_| {}).unwrap();
        assert!(prepared.others.is_none());
        assert!(!dir.path().join("job").join("others.wav").exists());
        assert_eq!(read_wav(&prepared.me).len(), 16_000);
    }

    #[test]
    fn a_file_that_is_not_opus_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.opus");
        std::fs::write(&bad, b"not ogg at all").unwrap();
        assert!(prepare(&bad, &dir.path().join("job"), |_| {}).is_err());
    }
}
