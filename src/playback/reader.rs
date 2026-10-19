//! Decodes the Ogg Opus files MeetRec writes (and any 1–2 channel Ogg Opus).
//!
//! Stereo recordings are two uncoupled mono streams (mapping family 1), which
//! some players cannot decode; libopus' multistream decoder handles every
//! mapping. Opening scans the file once for its length and a page index
//! (granule → byte offset), so seeking is a lookup plus 80 ms of pre-roll.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{bail, Context, Result};
use audiopus_sys as ffi;

use crate::audio::{FRAME, SAMPLE_RATE};

/// Decoding restarts this far before a seek target so the decoder converges (RFC 7845 §4.6).
const PREROLL: u64 = 3840;
/// Largest Opus packet: 120 ms at 48 kHz.
const MAX_FRAME: usize = 5760;

struct MsDecoder(*mut ffi::OpusMSDecoder);

// The decoder state is owned exclusively by one OpusFileReader.
unsafe impl Send for MsDecoder {}

impl Drop for MsDecoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_multistream_decoder_destroy(self.0) };
    }
}

pub struct OpusFileReader<R: Read + Seek> {
    packets: ogg::PacketReader<R>,
    dec: MsDecoder,
    channels: usize,
    pre_skip: u64,
    /// Granule position where the audio ends: that of the last complete page.
    end: u64,
    /// Per page that ends a packet: its granule position and the byte offset just after it.
    index: Vec<(u64, u64)>,
    /// Byte offset of the first audio page.
    data_start: u64,
    /// Granule position at the end of the last decoded packet.
    decoded: u64,
    /// Decoded samples before this granule position are dropped (pre-skip, pre-roll).
    skip_to: u64,
    /// Granule position of the next sample `next_chunk` returns.
    next: u64,
    pcm: Vec<f32>,
}

impl OpusFileReader<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::new(BufReader::new(file))
    }
}

impl<R: Read + Seek> OpusFileReader<R> {
    pub fn new(r: R) -> Result<Self> {
        let mut packets = ogg::PacketReader::new(r);
        let head = packets.read_packet()?.context("empty file")?;
        let layout = Layout::parse(&head.data)?;
        let tags = packets.read_packet()?.context("no OpusTags packet")?;
        if !tags.data.starts_with(b"OpusTags") {
            bail!("no OpusTags packet");
        }
        // OpusTags ends its page, so the audio starts on a page boundary here.
        let data_start = packets.get_mut().stream_position()?;
        let mut index = Vec::new();
        while let Ok(Some(p)) = packets.read_packet() {
            if p.last_in_page() {
                index.push((p.absgp_page(), packets.get_mut().stream_position()?));
            }
        }

        let mut err = 0;
        let dec = unsafe {
            ffi::opus_multistream_decoder_create(
                SAMPLE_RATE as i32,
                layout.channels as i32,
                layout.streams,
                layout.coupled,
                layout.mapping.as_ptr(),
                &mut err,
            )
        };
        if err != ffi::OPUS_OK || dec.is_null() {
            bail!("opus_multistream_decoder_create failed: {err}");
        }
        let mut reader = Self {
            packets,
            dec: MsDecoder(dec),
            channels: layout.channels,
            pre_skip: layout.pre_skip,
            end: index.last().map_or(0, |&(g, _)| g),
            index,
            data_start,
            decoded: 0,
            skip_to: 0,
            next: 0,
            pcm: vec![0.0; MAX_FRAME * layout.channels],
        };
        reader.seek(0.0)?;
        Ok(reader)
    }

    /// 1 (mono) or 2 (L = mic, R = system).
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn duration_secs(&self) -> f64 {
        self.end.saturating_sub(self.pre_skip) as f64 / SAMPLE_RATE as f64
    }

    /// Time of the next sample `next_chunk` returns.
    pub fn position_secs(&self) -> f64 {
        self.position_samples() as f64 / SAMPLE_RATE as f64
    }

    pub fn position_samples(&self) -> u64 {
        self.next.saturating_sub(self.pre_skip)
    }

    /// Moves to `secs` (clamped to the file), sample-accurately.
    pub fn seek(&mut self, secs: f64) -> Result<()> {
        let target = (self.pre_skip + (secs.max(0.0) * SAMPLE_RATE as f64).round() as u64).min(self.end);
        // Resume at the start of the page holding the pre-roll point.
        let from = target.saturating_sub(PREROLL);
        let (granule, offset) = match self.index.partition_point(|&(g, _)| g <= from) {
            0 => (0, self.data_start),
            i => self.index[i - 1],
        };
        self.packets.seek_bytes(SeekFrom::Start(offset))?;
        unsafe { ffi::opus_multistream_decoder_ctl(self.dec.0, ffi::OPUS_RESET_STATE) };
        self.decoded = granule;
        self.skip_to = target.max(self.pre_skip);
        self.next = self.skip_to;
        Ok(())
    }

    /// The next decoded samples (`channels()` interleaved), or `None` at the end.
    /// A damaged page ends the file, the way a crashed recording ends.
    pub fn next_chunk(&mut self) -> Option<&[f32]> {
        while self.decoded < self.end {
            let Ok(Some(packet)) = self.packets.read_packet() else { break };
            let start = self.decoded;
            self.decoded += self.decode(&packet.data) as u64;
            let (lo, hi) = (self.skip_to.max(start), self.decoded.min(self.end));
            if lo < hi {
                self.next = hi;
                let ch = self.channels;
                return Some(&self.pcm[(lo - start) as usize * ch..(hi - start) as usize * ch]);
            }
        }
        // Packets can run out before the granule says (damage, or files whose
        // granules counted pre-skip twice): the audio ends here.
        self.end = self.end.min(self.next);
        None
    }

    /// Decodes one packet into `pcm`; returns samples per channel. A corrupt
    /// packet becomes silence of its nominal length, keeping the timeline.
    fn decode(&mut self, data: &[u8]) -> usize {
        let n = unsafe {
            ffi::opus_multistream_decode_float(
                self.dec.0,
                data.as_ptr(),
                data.len() as i32,
                self.pcm.as_mut_ptr(),
                MAX_FRAME as i32,
                0,
            )
        };
        if n >= 0 {
            return n as usize;
        }
        let n = unsafe { ffi::opus_packet_get_nb_samples(data.as_ptr(), data.len() as i32, SAMPLE_RATE as i32) };
        let n = if n > 0 { (n as usize).min(MAX_FRAME) } else { FRAME };
        self.pcm[..n * self.channels].fill(0.0);
        n
    }
}

/// What the decoder needs from the OpusHead packet.
struct Layout {
    channels: usize,
    pre_skip: u64,
    streams: i32,
    coupled: i32,
    mapping: Vec<u8>,
}

impl Layout {
    fn parse(h: &[u8]) -> Result<Self> {
        if h.len() < 19 || &h[..8] != b"OpusHead" {
            bail!("not an Ogg Opus file");
        }
        let channels = h[9] as usize;
        let pre_skip = u16::from_le_bytes([h[10], h[11]]) as u64;
        let (streams, coupled, mapping) = match (h[18], channels) {
            (0, 1 | 2) => (1, channels as i32 - 1, [0u8, 1][..channels].to_vec()),
            (1, 1 | 2) if h.len() >= 21 + channels => (h[19] as i32, h[20] as i32, h[21..21 + channels].to_vec()),
            (family, n) => bail!("unsupported Opus layout: mapping family {family}, {n} channels"),
        };
        Ok(Self { channels, pre_skip, streams, coupled, mapping })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::opus_ogg::OggOpusWriter;
    use std::io::Cursor;

    const SR: f32 = SAMPLE_RATE as f32;

    /// Encodes `secs` seconds; `left`/`right` give each channel's sample at index `i`.
    fn encode(channels: u8, secs: usize, left: impl Fn(usize) -> f32, right: impl Fn(usize) -> f32) -> Vec<u8> {
        let mut w = OggOpusWriter::new(Vec::new(), channels, 48_000, &[]).unwrap();
        let mut n = 0;
        for _ in 0..secs * 50 {
            let mut pcm = Vec::with_capacity(FRAME * channels as usize);
            for _ in 0..FRAME {
                pcm.push(left(n));
                if channels == 2 {
                    pcm.push(right(n));
                }
                n += 1;
            }
            w.write_frame(&pcm).unwrap();
        }
        w.finish().unwrap()
    }

    fn sine(freq: f32, amp: f32) -> impl Fn(usize) -> f32 {
        move |i| (i as f32 * freq * 2.0 * std::f32::consts::PI / SR).sin() * amp
    }

    fn read_all<R: Read + Seek>(r: &mut OpusFileReader<R>) -> Vec<f32> {
        let mut out = Vec::new();
        while let Some(chunk) = r.next_chunk() {
            out.extend_from_slice(chunk);
        }
        out
    }

    fn channel(pcm: &[f32], channels: usize, ch: usize) -> Vec<f32> {
        pcm.iter().skip(ch).step_by(channels).copied().collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn stereo_file_decodes_to_its_exact_length_with_channels_kept_apart() {
        let mut r = OpusFileReader::new(Cursor::new(encode(2, 1, sine(440.0, 0.5), |_| 0.0))).unwrap();
        assert_eq!(r.channels(), 2);
        assert_eq!(r.duration_secs(), 1.0);
        let pcm = read_all(&mut r);
        assert_eq!(pcm.len(), 48_000 * 2);
        assert!(rms(&channel(&pcm, 2, 0)) > 0.3, "left carries the sine");
        assert!(rms(&channel(&pcm, 2, 1)) < 0.001, "right stays silent");
        assert_eq!(r.position_secs(), 1.0);
        assert!(r.next_chunk().is_none(), "stays at the end");
    }

    #[test]
    fn mono_file_decodes_as_one_channel() {
        let mut r = OpusFileReader::new(Cursor::new(encode(1, 1, sine(440.0, 0.5), |_| 0.0))).unwrap();
        assert_eq!(r.channels(), 1);
        let pcm = read_all(&mut r);
        assert_eq!(pcm.len(), 48_000);
        assert!(rms(&pcm) > 0.3);
    }

    #[test]
    fn seek_continues_exactly_where_linear_decoding_would_be() {
        // A 2 kHz tone: a one-sample misalignment would differ by ~0.13.
        let data = encode(2, 3, sine(2000.0, 0.5), sine(1000.0, 0.5));
        let linear = read_all(&mut OpusFileReader::new(Cursor::new(data.clone())).unwrap());
        let mut r = OpusFileReader::new(Cursor::new(data)).unwrap();
        for at in [2.37, 0.5, 0.01, 1.0] {
            r.seek(at).unwrap();
            assert!((r.position_secs() - at).abs() < 1.0 / SR as f64, "position {} after seek to {at}", r.position_secs());
            let mut got = Vec::new();
            while got.len() < FRAME * 2 * 5 {
                got.extend_from_slice(r.next_chunk().unwrap());
            }
            let start = (at * 48_000.0).round() as usize * 2;
            let want = &linear[start..start + got.len()];
            let diff: Vec<f32> = got.iter().zip(want).map(|(a, b)| a - b).collect();
            assert!(rms(&diff) < 0.1 * rms(want), "after seek to {at}: diff rms {} vs signal {}", rms(&diff), rms(want));
        }
    }

    #[test]
    fn seek_past_the_end_lands_at_the_end() {
        let mut r = OpusFileReader::new(Cursor::new(encode(1, 1, |_| 0.1, |_| 0.0))).unwrap();
        r.seek(99.0).unwrap();
        assert_eq!(r.position_secs(), 1.0);
        assert!(r.next_chunk().is_none());
        r.seek(-5.0).unwrap();
        assert_eq!(r.position_secs(), 0.0);
        assert_eq!(read_all(&mut r).len(), 48_000);
    }

    #[test]
    fn damaged_tail_plays_up_to_the_damage() {
        // Like a crashed recording: the last pages are cut off mid-page.
        let mut data = encode(2, 3, sine(440.0, 0.5), sine(440.0, 0.5));
        data.truncate(data.len() * 2 / 3);
        let mut r = OpusFileReader::new(Cursor::new(data)).unwrap();
        let secs = r.duration_secs();
        assert!((1.0..3.0).contains(&secs), "duration {secs}");
        let pcm = read_all(&mut r);
        assert_eq!(pcm.len(), (secs * 48_000.0).round() as usize * 2);
    }

    /// Rewrites every audio page's granule `extra` samples higher, like files
    /// from before the encoder counted pre-skip correctly.
    fn shift_granules(data: Vec<u8>, extra: u64) -> Vec<u8> {
        let mut r = ogg::PacketReader::new(Cursor::new(data));
        let mut w = ogg::PacketWriter::new(Vec::new());
        let mut n = 0;
        while let Some(p) = r.read_packet().unwrap() {
            let info = if p.last_in_stream() {
                ogg::PacketWriteEndInfo::EndStream
            } else if p.last_in_page() {
                ogg::PacketWriteEndInfo::EndPage
            } else {
                ogg::PacketWriteEndInfo::NormalPacket
            };
            let granule = if n < 2 { 0 } else { p.absgp_page() + extra };
            let serial = p.stream_serial();
            w.write_packet(p.data, serial, info, granule).unwrap();
            n += 1;
        }
        w.into_inner()
    }

    #[test]
    fn file_claiming_more_samples_than_it_holds_ends_where_the_audio_ends() {
        let data = shift_granules(encode(2, 1, sine(440.0, 0.5), |_| 0.0), 2_000);
        let mut r = OpusFileReader::new(Cursor::new(data)).unwrap();
        let pcm = read_all(&mut r);
        assert!(pcm.len() / 2 >= 47_000, "decoded {}", pcm.len() / 2);
        assert_eq!(r.position_secs(), r.duration_secs(), "the end is where decoding stopped");
        assert_eq!(r.duration_secs(), (pcm.len() / 2) as f64 / SR as f64);
    }

    #[test]
    fn rejects_files_that_are_not_opus() {
        assert!(OpusFileReader::new(Cursor::new(b"not an ogg file at all".to_vec())).is_err());
    }
}
