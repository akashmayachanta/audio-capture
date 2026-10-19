//! Turns a recording's channels (L = mic, R = system) into what both ears hear.

/// Which side of a stereo recording to listen to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Listen {
    /// Mic and system mixed into both ears, like hearing the meeting.
    #[default]
    Both,
    /// Left channel (mic) only.
    Me,
    /// Right channel (system audio) only.
    Others,
}

/// Appends `input` (`channels` interleaved, 1 or 2) to `out` as interleaved
/// stereo with the same signal in both ears. Mono input ignores `listen`.
pub fn to_stereo(input: &[f32], channels: usize, listen: Listen, volume: f32, out: &mut Vec<f32>) {
    out.reserve(input.len() / channels * 2);
    for frame in input.chunks_exact(channels) {
        let s = match (frame, listen) {
            ([mono], _) => *mono,
            ([l, r], Listen::Both) => l + r,
            ([l, _], Listen::Me) => *l,
            ([_, r], Listen::Others) => *r,
            _ => 0.0,
        };
        let s = (s * volume).clamp(-1.0, 1.0);
        out.push(s);
        out.push(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(input: &[f32], channels: usize, listen: Listen, volume: f32) -> Vec<f32> {
        let mut out = Vec::new();
        to_stereo(input, channels, listen, volume, &mut out);
        out
    }

    #[test]
    fn both_mixes_mic_and_system_into_both_ears() {
        assert_eq!(mix(&[0.25, 0.5, -0.25, 0.0], 2, Listen::Both, 1.0), vec![0.75, 0.75, -0.25, -0.25]);
    }

    #[test]
    fn both_clamps_instead_of_wrapping() {
        assert_eq!(mix(&[0.75, 0.5, -0.75, -0.5], 2, Listen::Both, 1.0), vec![1.0, 1.0, -1.0, -1.0]);
    }

    #[test]
    fn me_plays_only_the_left_channel() {
        assert_eq!(mix(&[0.25, 0.5], 2, Listen::Me, 1.0), vec![0.25, 0.25]);
    }

    #[test]
    fn others_plays_only_the_right_channel() {
        assert_eq!(mix(&[0.25, 0.5], 2, Listen::Others, 1.0), vec![0.5, 0.5]);
    }

    #[test]
    fn mono_goes_to_both_ears_whatever_the_listen_mode() {
        for listen in [Listen::Both, Listen::Me, Listen::Others] {
            assert_eq!(mix(&[0.25, -0.5], 1, listen, 1.0), vec![0.25, 0.25, -0.5, -0.5]);
        }
    }

    #[test]
    fn volume_scales_the_output() {
        assert_eq!(mix(&[0.5, 0.25], 2, Listen::Both, 0.5), vec![0.375, 0.375]);
    }

    #[test]
    fn appends_to_existing_output() {
        let mut out = vec![9.0];
        to_stereo(&[0.5], 1, Listen::Both, 1.0, &mut out);
        assert_eq!(out, vec![9.0, 0.5, 0.5]);
    }
}
