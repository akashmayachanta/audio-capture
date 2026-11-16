//! Ogg Opus writer on top of libopus' multistream encoder.
//!
//! Stereo files use channel mapping family 1 with two *uncoupled* mono streams, so
//! the encoder can never blend left (mic) and right (system) the way joint/intensity
//! stereo does at low bitrates. Mono files use mapping family 0.

use std::io::Write;

use anyhow::{bail, Result};
use audiopus_sys as ffi;
use ogg::PacketWriteEndInfo;

use crate::audio::{FRAME, SAMPLE_RATE};

/// Packets per Ogg page: 50 x 20 ms = one page (and one flush) per second.
const PACKETS_PER_PAGE: u32 = 50;
const MAX_PACKET: usize = 4000;

struct MsEncoder(*mut ffi::OpusMSEncoder);

// The encoder state is owned exclusively by one OggOpusWriter.
unsafe impl Send for MsEncoder {}

impl Drop for MsEncoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_multistream_encoder_destroy(self.0) };
    }
}

pub struct OggOpusWriter<W: Write> {
    writer: ogg::PacketWriter<'static, W>,
    enc: MsEncoder,
    serial: u32,
    channels: usize,
    pre_skip: u64,
    samples: u64,
    packets_in_page: u32,
    /// The newest packet is held back so `finish` can mark it end-of-stream.
    held: Option<(Vec<u8>, u64)>,
    buf: Vec<u8>,
}

impl<W: Write> OggOpusWriter<W> {
    /// `channels` is 1 or 2; `bitrate_per_channel` is in bits per second.
    pub fn new(w: W, channels: u8, bitrate_per_channel: i32, tags: &[(&str, &str)]) -> Result<Self> {
        if !(1..=2).contains(&channels) {
            bail!("unsupported channel count {channels}");
        }
        let mapping = [0u8, 1];
        let mut err = 0;
        let enc = unsafe {
            ffi::opus_multistream_encoder_create(
                SAMPLE_RATE as i32,
                channels as i32,
                channels as i32, // one mono stream per channel
                0,               // none coupled
                mapping.as_ptr(),
                ffi::OPUS_APPLICATION_VOIP,
                &mut err,
            )
        };
        if err != ffi::OPUS_OK || enc.is_null() {
            bail!("opus_multistream_encoder_create failed: {err}");
        }
        let enc = MsEncoder(enc);
        let mut lookahead: i32 = 0;
        unsafe {
            ffi::opus_multistream_encoder_ctl(enc.0, ffi::OPUS_SET_BITRATE_REQUEST, bitrate_per_channel * channels as i32);
            ffi::opus_multistream_encoder_ctl(enc.0, ffi::OPUS_SET_SIGNAL_REQUEST, ffi::OPUS_SIGNAL_VOICE);
            ffi::opus_multistream_encoder_ctl(enc.0, ffi::OPUS_GET_LOOKAHEAD_REQUEST, &mut lookahead as *mut i32);
        }

        let serial = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ d.as_secs() as u32)
            .unwrap_or(0x6d72_6563);
        let mut writer = ogg::PacketWriter::new(w);
        writer.write_packet(opus_head(channels, lookahead as u16), serial, PacketWriteEndInfo::EndPage, 0)?;
        writer.write_packet(opus_tags(tags), serial, PacketWriteEndInfo::EndPage, 0)?;
        writer.inner_mut().flush()?;

        Ok(Self {
            writer,
            enc,
            serial,
            channels: channels as usize,
            pre_skip: lookahead as u64,
            samples: 0,
            packets_in_page: 0,
            held: None,
            buf: vec![0; MAX_PACKET * channels as usize],
        })
    }

    /// Encodes one 20 ms frame: 960 samples per channel, interleaved.
    pub fn write_frame(&mut self, pcm: &[f32]) -> Result<()> {
        if pcm.len() != FRAME * self.channels {
            bail!("frame has {} samples, expected {}", pcm.len(), FRAME * self.channels);
        }
        let len = unsafe {
            ffi::opus_multistream_encode_float(
                self.enc.0,
                pcm.as_ptr(),
                FRAME as i32,
                self.buf.as_mut_ptr(),
                self.buf.len() as i32,
            )
        };
        if len < 0 {
            bail!("opus encode failed: {len}");
        }
        self.samples += FRAME as u64;
        let packet = self.buf[..len as usize].to_vec();
        // Granule = samples decoded through this packet, pre-skip included (RFC 7845 §4).
        if let Some((prev, granule)) = self.held.replace((packet, self.samples)) {
            self.packets_in_page += 1;
            let end_page = self.packets_in_page >= PACKETS_PER_PAGE;
            let info = if end_page { PacketWriteEndInfo::EndPage } else { PacketWriteEndInfo::NormalPacket };
            self.writer.write_packet(prev, self.serial, info, granule)?;
            if end_page {
                self.packets_in_page = 0;
                self.writer.inner_mut().flush()?;
            }
        }
        Ok(())
    }

    /// Samples per channel written so far (excluding pre-skip).
    pub fn samples_written(&self) -> u64 {
        self.samples
    }

    /// Writes the end-of-stream page and returns the inner writer, flushed.
    pub fn finish(mut self) -> Result<W> {
        let real = self.samples;
        // The encoder still holds the last pre-skip's worth of audio in its
        // lookahead: one frame of silence pushes it out, and the final granule
        // trims the excess. This also gives an empty stream a packet to carry EOS.
        self.write_frame(&vec![0.0; FRAME * self.channels])?;
        let (packet, _) = self.held.take().expect("held packet");
        let granule = self.pre_skip + real;
        self.writer.write_packet(packet, self.serial, PacketWriteEndInfo::EndStream, granule)?;
        let mut w = self.writer.into_inner();
        w.flush()?;
        Ok(w)
    }
}

fn opus_head(channels: u8, pre_skip: u16) -> Vec<u8> {
    let mut h = Vec::with_capacity(21);
    h.extend_from_slice(b"OpusHead");
    h.push(1); // version
    h.push(channels);
    h.extend_from_slice(&pre_skip.to_le_bytes());
    h.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes()); // output gain
    if channels == 1 {
        h.push(0); // family 0: mono, no mapping table
    } else {
        h.push(1); // family 1
        h.push(channels); // stream count
        h.push(0); // coupled count
        h.extend(0..channels); // channel i -> stream i
    }
    h
}

fn opus_tags(tags: &[(&str, &str)]) -> Vec<u8> {
    let vendor = concat!("meetrec ", env!("CARGO_PKG_VERSION"));
    let mut t = Vec::new();
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    t.extend_from_slice(vendor.as_bytes());
    t.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let c = format!("{k}={v}");
        t.extend_from_slice(&(c.len() as u32).to_le_bytes());
        t.extend_from_slice(c.as_bytes());
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;
    use audiopus_sys as ffi;
    use std::io::Cursor;

    fn sine(i: usize) -> f32 {
        (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 48_000.0).sin() * 0.5
    }

    /// One second of stereo: 440 Hz sine on `loud` channel, silence on the other.
    fn encode_stereo(loud: usize, frames: usize) -> Vec<u8> {
        let mut w = OggOpusWriter::new(Vec::new(), 2, 32_000, &[("TITLE", "test")]).unwrap();
        let mut n = 0;
        for _ in 0..frames {
            let mut pcm = vec![0.0f32; FRAME * 2];
            for f in 0..FRAME {
                pcm[f * 2 + loud] = sine(n);
                n += 1;
            }
            w.write_frame(&pcm).unwrap();
        }
        w.finish().unwrap()
    }

    fn packets(data: Vec<u8>) -> Vec<ogg::Packet> {
        let mut r = ogg::PacketReader::new(Cursor::new(data));
        let mut out = Vec::new();
        while let Some(p) = r.read_packet().unwrap() {
            out.push(p);
        }
        out
    }

    fn pre_skip(head: &[u8]) -> u64 {
        u16::from_le_bytes([head[10], head[11]]) as u64
    }

    #[test]
    fn stereo_header_uses_two_uncoupled_streams() {
        let p = packets(encode_stereo(0, 5));
        let head = &p[0].data;
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(head[8], 1, "version");
        assert_eq!(head[9], 2, "channels");
        assert_eq!(u32::from_le_bytes(head[12..16].try_into().unwrap()), 48_000);
        assert_eq!(head[18], 1, "mapping family");
        assert_eq!(head[19], 2, "stream count");
        assert_eq!(head[20], 0, "coupled count");
        assert_eq!(&head[21..23], &[0, 1], "channel mapping");
        assert!(p[0].last_in_page(), "OpusHead must be alone on the first page");
        assert_eq!(&p[1].data[..8], b"OpusTags");
        assert!(p[1].last_in_page());
        let tags = String::from_utf8_lossy(&p[1].data);
        assert!(tags.contains("TITLE=test"));
    }

    #[test]
    fn mono_header_uses_family_zero() {
        let mut w = OggOpusWriter::new(Vec::new(), 1, 32_000, &[]).unwrap();
        w.write_frame(&vec![0.0; FRAME]).unwrap();
        let p = packets(w.finish().unwrap());
        assert_eq!(p[0].data[9], 1);
        assert_eq!(p[0].data[18], 0);
        assert_eq!(p[0].data.len(), 19);
    }

    #[test]
    fn granule_positions_are_monotonic_and_end_at_total_samples() {
        let frames = 120;
        let p = packets(encode_stereo(0, frames));
        let skip = pre_skip(&p[0].data);
        let audio = &p[2..];
        assert_eq!(audio.len(), frames + 1, "one extra packet flushes the encoder's lookahead");
        let mut last = 0;
        for pk in audio.iter().filter(|pk| pk.last_in_page()) {
            assert!(pk.absgp_page() > last);
            last = pk.absgp_page();
        }
        let end = audio.last().unwrap();
        assert!(end.last_in_stream());
        assert_eq!(end.absgp_page(), skip + (frames * FRAME) as u64);
    }

    #[test]
    fn page_granules_count_decoded_samples_including_pre_skip() {
        // RFC 7845 §4: a page's granule position is the number of samples decoded
        // through its last packet, pre-skip included. Seeking relies on it.
        let p = packets(encode_stereo(0, 120));
        let audio = &p[2..];
        let mut decoded = 0;
        for pk in &audio[..audio.len() - 1] {
            decoded += FRAME as u64;
            if pk.last_in_page() {
                assert_eq!(pk.absgp_page(), decoded);
            }
        }
    }

    #[test]
    fn the_last_samples_are_not_lost_in_the_encoder() {
        // The final pre-skip's worth of audio still sits in the encoder's
        // lookahead at finish; it must be flushed out, not dropped.
        let frames = 50;
        let p = packets(encode_stereo(0, frames));
        let skip = pre_skip(&p[0].data) as usize;
        let end = p.last().unwrap().absgp_page() as usize;
        let pcm = decode_all(&p);
        assert!(pcm.len() / 2 >= end, "decoded {} samples, granule says {end}", pcm.len() / 2);
        let tail: Vec<f32> = pcm[(end - skip / 2) * 2..end * 2].iter().step_by(2).copied().collect();
        let tail_rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(tail_rms > 0.2, "the final samples are silent (rms {tail_rms})");
    }

    #[test]
    fn pages_are_flushed_about_every_second_before_finish() {
        let mut w = OggOpusWriter::new(Vec::new(), 2, 32_000, &[]).unwrap();
        for _ in 0..60 {
            w.write_frame(&vec![0.1; FRAME * 2]).unwrap();
        }
        assert_eq!(w.samples_written(), (60 * FRAME) as u64);
        // Peek at the bytes written so far, without finishing the stream.
        let partial = w.writer.inner().clone();
        let p = packets(partial);
        assert!(p.len() >= 2 + 50, "only {} packets readable", p.len());
    }

    /// Every sample the stereo packets decode to (pre-skip included), interleaved.
    fn decode_all(p: &[ogg::Packet]) -> Vec<f32> {
        let mut err = 0;
        let mapping = [0u8, 1];
        let dec = unsafe { ffi::opus_multistream_decoder_create(48_000, 2, 2, 0, mapping.as_ptr(), &mut err) };
        assert_eq!(err, ffi::OPUS_OK);
        let mut out = Vec::new();
        let mut pcm = vec![0f32; 5760 * 2];
        for pk in &p[2..] {
            let got = unsafe {
                ffi::opus_multistream_decode_float(dec, pk.data.as_ptr(), pk.data.len() as i32, pcm.as_mut_ptr(), 5760, 0)
            };
            assert!(got > 0);
            out.extend_from_slice(&pcm[..got as usize * 2]);
        }
        unsafe { ffi::opus_multistream_decoder_destroy(dec) };
        out
    }

    fn decode_rms(data: Vec<u8>) -> (f32, f32) {
        let pcm = decode_all(&packets(data));
        let n = (pcm.len() / 2) as f64;
        let power = |ch: usize| pcm.iter().skip(ch).step_by(2).map(|&s| (s as f64).powi(2)).sum::<f64>();
        ((power(0) / n).sqrt() as f32, (power(1) / n).sqrt() as f32)
    }

    #[test]
    fn left_only_input_stays_on_the_left() {
        let (l, r) = decode_rms(encode_stereo(0, 50));
        assert!(l > 0.2, "left rms {l}");
        assert!(r < 0.001, "right rms {r} should be silent");
    }

    #[test]
    fn right_only_input_stays_on_the_right() {
        let (l, r) = decode_rms(encode_stereo(1, 50));
        assert!(r > 0.2, "right rms {r}");
        assert!(l < 0.001, "left rms {l} should be silent");
    }
}
