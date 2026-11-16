//! Pulls 20 ms frames from the mic and system ring buffers and interleaves them
//! (L = mic, R = system). The mic clock is the master; the system (loopback)
//! buffer is a small jitter buffer whose drift is corrected by dropping samples
//! when it grows and inserting silence when it starves.

use ringbuf::HeapCons;
use ringbuf::traits::{Consumer, Observer};

use super::FRAME;

/// System audio is held back until this much is buffered (40 ms of jitter headroom).
pub const PRIME: usize = FRAME * 2;
/// Above this the system clock is running fast relative to the mic: start dropping.
pub const HIGH_WATER: usize = FRAME * 6;
/// Samples dropped per frame while above HIGH_WATER (1 ms, inaudible).
pub const DRIFT_STEP: usize = 48;
/// A backlog this large (1 s) is a burst, not drift: cut straight back to PRIME.
pub const HARD_LIMIT: usize = FRAME * 50;

/// Jitter buffer for the non-master (system) source.
struct Slave {
    cons: HeapCons<f32>,
    primed: bool,
}

impl Slave {
    /// Fills `buf` with one frame, correcting drift first. Missing samples are zero.
    fn read(&mut self, buf: &mut [f32]) {
        let occupied = self.cons.occupied_len();
        if !self.primed {
            if occupied < PRIME {
                buf.fill(0.0);
                return;
            }
            self.primed = true;
        }
        if occupied > HARD_LIMIT {
            self.cons.skip(occupied - (PRIME + FRAME));
        } else if occupied > HIGH_WATER {
            self.cons.skip(DRIFT_STEP);
        }
        let got = self.cons.pop_slice(buf);
        if got < buf.len() {
            buf[got..].fill(0.0);
            self.primed = false;
        }
    }
}

pub struct Mixer {
    master: HeapCons<f32>,
    slave: Option<Slave>,
    master_buf: Vec<f32>,
    slave_buf: Vec<f32>,
}

impl Mixer {
    /// At least one source must be present. With both, the mic is the master clock.
    pub fn new(mic: Option<HeapCons<f32>>, sys: Option<HeapCons<f32>>) -> Self {
        let (master, slave) = match (mic, sys) {
            (Some(m), s) => (m, s),
            (None, Some(s)) => (s, None),
            (None, None) => panic!("mixer needs at least one source"),
        };
        Self {
            master,
            slave: slave.map(|cons| Slave { cons, primed: false }),
            master_buf: vec![0.0; FRAME],
            slave_buf: vec![0.0; FRAME],
        }
    }

    pub fn channels(&self) -> usize {
        if self.slave.is_some() { 2 } else { 1 }
    }

    /// Writes one 20 ms frame (interleaved when stereo) into `out`, replacing its
    /// contents. Returns false, consuming nothing, when the master is short.
    pub fn next_frame(&mut self, out: &mut Vec<f32>) -> bool {
        if self.master.occupied_len() < FRAME {
            return false;
        }
        self.master.pop_slice(&mut self.master_buf);
        out.clear();
        match &mut self.slave {
            None => out.extend_from_slice(&self.master_buf),
            Some(slave) => {
                slave.read(&mut self.slave_buf);
                for (m, s) in self.master_buf.iter().zip(&self.slave_buf) {
                    out.push(*m);
                    out.push(*s);
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::FRAME;
    use ringbuf::traits::{Producer, Split};
    use ringbuf::{HeapProd, HeapRb};

    fn source() -> (HeapProd<f32>, HeapCons<f32>) {
        HeapRb::<f32>::new(48_000 * 4).split()
    }

    fn push(p: &mut HeapProd<f32>, value: f32, n: usize) {
        for _ in 0..n {
            p.try_push(value).unwrap();
        }
    }

    fn left(out: &[f32]) -> Vec<f32> {
        out.iter().step_by(2).copied().collect()
    }

    fn right(out: &[f32]) -> Vec<f32> {
        out.iter().skip(1).step_by(2).copied().collect()
    }

    #[test]
    fn both_sources_interleave_mic_left_system_right() {
        let (mut mp, mc) = source();
        let (mut sp, sc) = source();
        push(&mut mp, 0.5, FRAME);
        push(&mut sp, 0.25, PRIME);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert_eq!(m.channels(), 2);
        assert!(m.next_frame(&mut out));
        assert_eq!(out.len(), FRAME * 2);
        assert!(left(&out).iter().all(|&s| s == 0.5));
        assert!(right(&out).iter().all(|&s| s == 0.25));
    }

    #[test]
    fn no_frame_until_master_has_a_full_frame() {
        let (mut mp, mc) = source();
        let (mut sp, sc) = source();
        push(&mut mp, 0.5, FRAME - 1);
        push(&mut sp, 0.25, PRIME);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert!(!m.next_frame(&mut out));
        assert_eq!(sp.occupied_len(), PRIME, "system must not be consumed");
    }

    #[test]
    fn starved_system_is_silence() {
        let (mut mp, mc) = source();
        let (_sp, sc) = source();
        push(&mut mp, 0.5, FRAME);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert!(m.next_frame(&mut out));
        assert!(left(&out).iter().all(|&s| s == 0.5));
        assert!(right(&out).iter().all(|&s| s == 0.0));
    }

    #[test]
    fn system_is_not_consumed_until_primed() {
        let (mut mp, mc) = source();
        let (mut sp, sc) = source();
        push(&mut mp, 0.5, FRAME);
        push(&mut sp, 0.25, 500);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert!(m.next_frame(&mut out));
        assert!(right(&out).iter().all(|&s| s == 0.0));
        assert_eq!(sp.occupied_len(), 500);
    }

    #[test]
    fn underrun_after_priming_pads_with_zeros() {
        let (mut mp, mc) = source();
        let (mut sp, sc) = source();
        push(&mut mp, 0.5, FRAME * 3);
        push(&mut sp, 0.25, PRIME + 500);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert!(m.next_frame(&mut out)); // primed, consumes FRAME
        assert!(m.next_frame(&mut out)); // consumes FRAME, 500 left
        assert!(m.next_frame(&mut out)); // underrun: 500 real samples then zeros
        let r = right(&out);
        assert!(r[..500].iter().all(|&s| s == 0.25));
        assert!(r[500..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn growing_system_buffer_drops_a_few_samples_per_frame() {
        let (mut mp, mc) = source();
        let (mut sp, sc) = source();
        push(&mut mp, 0.5, FRAME);
        let start = HIGH_WATER + FRAME + 2_000;
        push(&mut sp, 0.25, start);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert!(m.next_frame(&mut out));
        assert_eq!(sp.occupied_len(), start - FRAME - DRIFT_STEP);
    }

    #[test]
    fn huge_system_backlog_is_cut_back_to_prime_level() {
        let (mut mp, mc) = source();
        let (mut sp, sc) = source();
        push(&mut mp, 0.5, FRAME);
        push(&mut sp, 0.25, HARD_LIMIT + 10_000);
        let mut m = Mixer::new(Some(mc), Some(sc));
        let mut out = Vec::new();
        assert!(m.next_frame(&mut out));
        assert_eq!(sp.occupied_len(), PRIME);
    }

    #[test]
    fn mic_only_is_mono() {
        let (mut mp, mc) = source();
        push(&mut mp, 0.5, FRAME);
        let mut m = Mixer::new(Some(mc), None);
        let mut out = Vec::new();
        assert_eq!(m.channels(), 1);
        assert!(m.next_frame(&mut out));
        assert_eq!(out, vec![0.5; FRAME]);
    }

    #[test]
    fn system_only_is_mono_with_system_as_master() {
        let (mut sp, sc) = source();
        push(&mut sp, 0.25, FRAME);
        let mut m = Mixer::new(None, Some(sc));
        let mut out = Vec::new();
        assert_eq!(m.channels(), 1);
        assert!(m.next_frame(&mut out));
        assert_eq!(out, vec![0.25; FRAME]);
        assert!(!m.next_frame(&mut out));
    }
}
