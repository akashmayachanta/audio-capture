//! One WASAPI capture stream per source, each on its own thread, pushing mono
//! 48 kHz f32 into a ring buffer.
//!
//! The mic is a capture endpoint. System audio is the default render endpoint
//! opened in loopback mode. Both use shared-mode *polling*: loopback delivers no
//! packets at all while nothing is playing, so each stream tracks wall-clock time
//! and fills stalls with silence to stay continuous. A failed stream (device
//! unplugged, default changed) is reopened on the current default device, writing
//! into the same ring buffer, so the recording carries on in the same file.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use ringbuf::traits::{Observer, Producer};
use ringbuf::HeapProd;
use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

use super::convert::{silence_needed, Converter, SampleFormat};
use super::{device, SAMPLE_RATE};

const POLL: Duration = Duration::from_millis(10);
const RETRY: Duration = Duration::from_millis(500);
/// 200 ms device buffer: plenty of slack for a 10 ms poll.
const BUFFER_HNS: i64 = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Mic,
    System,
}

struct Shared {
    stop: AtomicBool,
    restart: AtomicBool,
    device_name: Mutex<String>,
}

pub struct CaptureStream {
    source: Source,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl CaptureStream {
    /// `device_id` picks a specific endpoint (mic, or render endpoint for
    /// System loopback); `None` follows the default.
    pub fn spawn(source: Source, device_id: Option<String>, prod: HeapProd<f32>) -> Self {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            restart: AtomicBool::new(false),
            device_name: Mutex::new(String::new()),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("capture-{source:?}").to_lowercase())
            .spawn(move || capture_thread(source, device_id, prod, &s))
            .expect("spawn capture thread");
        Self { source, shared, thread: Some(thread) }
    }

    /// Reopen the stream on the (possibly new) default device.
    pub fn request_restart(&self) {
        self.shared.restart.store(true, Ordering::Relaxed);
    }

    pub fn source(&self) -> Source {
        self.source
    }

    pub fn device_name(&self) -> String {
        self.shared.device_name.lock().unwrap().clone()
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Detects stalls and keeps the stream continuous across them.
///
/// Every packet from the device (silent-flagged ones included) re-anchors the
/// clock, so only time since the *last* packet counts. Comparing against the
/// wall clock since start instead would mistake ordinary device/QPC clock
/// drift for a stall and sprinkle silence into speech on long recordings.
/// The anchor survives device restarts, so a reopen gap becomes silence.
struct Clock {
    anchor: Instant,
    /// `delivered` at the anchor.
    anchor_delivered: u64,
    delivered: u64,
    silence: Vec<f32>,
}

impl Clock {
    fn new(now: Instant) -> Self {
        Self { anchor: now, anchor_delivered: 0, delivered: 0, silence: Vec::new() }
    }

    /// Device data (real or silent-flagged) as of now.
    fn push(&mut self, prod: &mut HeapProd<f32>, samples: &[f32]) {
        self.push_at(prod, samples, Instant::now());
    }

    fn push_at(&mut self, prod: &mut HeapProd<f32>, samples: &[f32], now: Instant) {
        self.write(prod, samples);
        self.anchor = now;
        self.anchor_delivered = self.delivered;
    }

    fn push_silence(&mut self, prod: &mut HeapProd<f32>, n: usize) {
        self.push_silence_at(prod, n, Instant::now());
    }

    fn push_silence_at(&mut self, prod: &mut HeapProd<f32>, n: usize, now: Instant) {
        let silence = self.take_silence(n);
        self.push_at(prod, &silence, now);
        self.silence = silence;
    }

    fn fill_gap(&mut self, prod: &mut HeapProd<f32>) {
        self.fill_gap_at(prod, Instant::now());
    }

    /// Pads with silence when nothing has arrived for longer than `GAP`.
    fn fill_gap_at(&mut self, prod: &mut HeapProd<f32>, now: Instant) {
        let since = now.duration_since(self.anchor).as_secs_f64();
        let expected = self.anchor_delivered + (since * SAMPLE_RATE as f64) as u64;
        let n = silence_needed(expected, self.delivered);
        if n > 0 {
            // Padding is not device data: it must not move the anchor. Only write
            // what the ring can hold (a stall can be hours long after sleep), but
            // account for all of it so the next poll does not pad again.
            let silence = self.take_silence(n.min(prod.vacant_len()));
            self.write(prod, &silence);
            self.delivered += (n - silence.len()) as u64;
            self.silence = silence;
        }
    }

    fn take_silence(&mut self, n: usize) -> Vec<f32> {
        let mut silence = std::mem::take(&mut self.silence);
        silence.clear();
        silence.resize(n, 0.0);
        silence
    }

    fn write(&mut self, prod: &mut HeapProd<f32>, samples: &[f32]) {
        // A full ring means nobody is draining it; dropping is the only option.
        prod.push_slice(samples);
        self.delivered += samples.len() as u64;
    }
}

fn capture_thread(source: Source, device_id: Option<String>, mut prod: HeapProd<f32>, shared: &Shared) {
    if wasapi::initialize_mta().is_err() {
        tracing::error!("{source:?}: COM init failed");
        return;
    }
    let mut clock = Clock::new(Instant::now());
    while !shared.stop.load(Ordering::Relaxed) {
        shared.restart.store(false, Ordering::Relaxed);
        if let Err(e) = run_stream(source, device_id.as_deref(), &mut prod, shared, &mut clock) {
            tracing::warn!("{source:?} capture failed: {e:#}; retrying");
            shared.device_name.lock().unwrap().clear();
            let until = Instant::now() + RETRY;
            while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
                clock.fill_gap(&mut prod);
                std::thread::sleep(POLL);
            }
        }
    }
    wasapi::deinitialize();
}

/// Captures until stop/restart is requested (Ok) or the device fails (Err).
fn run_stream(
    source: Source,
    device_id: Option<&str>,
    prod: &mut HeapProd<f32>,
    shared: &Shared,
    clock: &mut Clock,
) -> Result<()> {
    let device = match source {
        Source::Mic => device::open(Direction::Capture, device_id)?,
        Source::System => device::open(Direction::Render, device_id)?,
    };
    let name = device.get_friendlyname().unwrap_or_default();
    let mut client = device.get_iaudioclient()?;

    // Preferred: let Windows convert to 48 kHz mono float.
    let wanted = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 1, None);
    let auto = StreamMode::PollingShared { autoconvert: true, buffer_duration_hns: BUFFER_HNS };
    let (mut converter, block_align, rate) = match client.initialize_client(&wanted, &Direction::Capture, &auto) {
        Ok(()) => (Converter::new(SampleFormat::F32, 1, SAMPLE_RATE)?, wanted.get_blockalign(), SAMPLE_RATE),
        Err(e) => {
            tracing::info!("{source:?}: autoconvert refused ({e}), converting in-process");
            client = device.get_iaudioclient()?;
            let mix = client.get_mixformat()?;
            let format = match (mix.get_subformat()?, mix.get_bitspersample()) {
                (SampleType::Float, 32) => SampleFormat::F32,
                (SampleType::Int, 16) => SampleFormat::I16,
                (SampleType::Int, 32) => SampleFormat::I32,
                (t, b) => bail!("unsupported mix format {t:?}/{b}"),
            };
            let raw = StreamMode::PollingShared { autoconvert: false, buffer_duration_hns: BUFFER_HNS };
            client.initialize_client(&mix, &Direction::Capture, &raw)?;
            let rate = mix.get_samplespersec();
            (Converter::new(format, mix.get_nchannels() as usize, rate)?, mix.get_blockalign(), rate)
        }
    };
    let block_align = block_align as usize;
    let capture = client.get_audiocaptureclient()?;
    let mut bytes = vec![0u8; client.get_buffer_size()? as usize * block_align];
    let mut samples = Vec::new();

    client.start_stream()?;
    tracing::info!("{source:?}: capturing from {name}");
    *shared.device_name.lock().unwrap() = name;

    let result = (|| -> Result<()> {
        while !shared.stop.load(Ordering::Relaxed) && !shared.restart.load(Ordering::Relaxed) {
            while let Some(frames) = capture.get_next_packet_size()?.filter(|&n| n > 0) {
                let need = frames as usize * block_align;
                if bytes.len() < need {
                    bytes.resize(need, 0);
                }
                let (got, info) = capture.read_from_device(&mut bytes)?;
                if info.flags.silent {
                    let n = (got as u64 * SAMPLE_RATE as u64 / rate as u64) as usize;
                    clock.push_silence(prod, n);
                } else {
                    samples.clear();
                    converter.push(&bytes[..got as usize * block_align], &mut samples)?;
                    clock.push(prod, &samples);
                }
            }
            clock.fill_gap(prod);
            std::thread::sleep(POLL);
        }
        Ok(())
    })();
    let _ = client.stop_stream();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::{Observer, Split};
    use ringbuf::HeapRb;

    fn zeros(cons: &ringbuf::HeapCons<f32>) -> usize {
        use ringbuf::traits::Consumer;
        cons.iter().filter(|&&s| s == 0.0).count()
    }

    /// Feeds `secs` of 10 ms packets of `per_packet` samples, polling the gap filler after each.
    fn feed(clock: &mut Clock, prod: &mut HeapProd<f32>, t: &mut Instant, secs: u64, per_packet: usize) {
        let packet = vec![0.5f32; per_packet];
        for _ in 0..secs * 100 {
            *t += Duration::from_millis(10);
            clock.push_at(prod, &packet, *t);
            clock.fill_gap_at(prod, *t);
        }
    }

    #[test]
    fn slow_device_clock_never_gets_silence_inserted() {
        // A device running 1% slow against the wall clock (475 instead of 480 samples per 10 ms)
        // is still delivering continuously; padding it would put dropouts into speech.
        let (mut prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 70).split();
        let mut t = Instant::now();
        let mut clock = Clock::new(t);
        feed(&mut clock, &mut prod, &mut t, 60, 475);
        assert_eq!(zeros(&cons), 0);
    }

    #[test]
    fn a_real_stall_is_filled_with_silence() {
        let (mut prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 10).split();
        let mut t = Instant::now();
        let mut clock = Clock::new(t);
        feed(&mut clock, &mut prod, &mut t, 1, 480);
        for _ in 0..50 {
            // 500 ms with no packets at all
            t += Duration::from_millis(10);
            clock.fill_gap_at(&mut prod, t);
        }
        let ms = zeros(&cons) as f64 / 48.0;
        assert!((400.0..=480.0).contains(&ms), "{ms} ms of silence for a 500 ms stall");
    }

    #[test]
    fn long_stall_allocates_no_more_silence_than_the_ring_can_take() {
        // A capture thread that survives an overnight sleep sees hours of "stall";
        // padding must not try to allocate all of it.
        let cap = 4_800;
        let (mut prod, cons) = HeapRb::<f32>::new(cap).split();
        let mut t = Instant::now();
        let mut clock = Clock::new(t);
        clock.push_at(&mut prod, &[0.5; 480], t);
        t += Duration::from_secs(10);
        clock.fill_gap_at(&mut prod, t);
        assert!(clock.silence.capacity() <= cap, "allocated {} samples", clock.silence.capacity());
        assert_eq!(cons.occupied_len(), cap);
        assert!(clock.delivered >= 10 * 48_000 - 960, "the whole gap is accounted for: {}", clock.delivered);
        // ...so the next poll does not pad again.
        let before = clock.delivered;
        clock.fill_gap_at(&mut prod, t + Duration::from_millis(10));
        assert_eq!(clock.delivered, before);
    }

    #[test]
    fn silent_flagged_packets_count_as_data() {
        let (mut prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 10).split();
        let mut t = Instant::now();
        let mut clock = Clock::new(t);
        for _ in 0..100 {
            t += Duration::from_millis(10);
            clock.push_silence_at(&mut prod, 480, t);
            clock.fill_gap_at(&mut prod, t);
        }
        assert_eq!(cons.occupied_len(), 48_000, "no extra padding on top of device silence");
    }

    /// Needs real audio endpoints: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn stream_stays_continuous_across_a_restart() {
        for source in [Source::Mic, Source::System] {
            let (prod, cons) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 10).split();
            let stream = CaptureStream::spawn(source, None, prod);
            std::thread::sleep(Duration::from_millis(1000));
            assert!(!stream.device_name().is_empty(), "{source:?} never opened");
            stream.request_restart();
            std::thread::sleep(Duration::from_millis(1000));
            assert!(!stream.device_name().is_empty(), "{source:?} did not reopen");
            drop(stream);
            let got = cons.occupied_len() as f64 / SAMPLE_RATE as f64;
            assert!((1.8..=2.2).contains(&got), "{source:?}: {got:.3} s of audio for 2 s of wall time");
        }
    }
}
