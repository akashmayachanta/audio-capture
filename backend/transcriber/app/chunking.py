"""Cuts one channel into speech pieces at pauses (pure, no model).

Each piece becomes one transcript line, so its start time is the line's
timestamp. Silent stretches are dropped, which is most of the mic channel while
others talk and saves the model the work. A piece never exceeds `max_secs`
(Bark was evaluated on short clips; its memory grows with length): a long
monologue is cut at its quietest frame.
"""
from dataclasses import dataclass

import numpy as np

FRAME_SECS = 0.02


@dataclass(frozen=True)
class Piece:
    start: int  # samples
    end: int

    def secs(self, rate):
        return self.start / rate, self.end / rate


def frame_energy(samples, frame):
    """RMS per `frame` samples (last partial frame included)."""
    n = -(-len(samples) // frame)
    padded = np.zeros(n * frame, np.float32)
    padded[:len(samples)] = samples
    return np.sqrt(np.mean(padded.reshape(n, frame) ** 2, axis=1))


def speech_threshold(energy, floor=0.004):
    """Above the noise floor of this channel: a quiet room on one mic is not
    another's, so it adapts, but never drops below `floor`. Capped well under
    the loud frames, so a channel that is speech throughout (no quiet frames to
    measure the noise from) is not thrown away."""
    if len(energy) == 0:
        return floor
    noise, loud = np.percentile(energy, [10, 90])
    return max(floor, min(float(noise) * 3.0, float(loud) * 0.3))


def plan(samples, rate=16000, max_secs=20.0, min_pause_secs=0.6, pad_secs=0.2, min_speech_secs=0.3):
    """-> [Piece] covering the speech in `samples`."""
    frame = int(rate * FRAME_SECS)
    energy = frame_energy(samples, frame)
    voiced = energy > speech_threshold(energy)
    max_frames = int(max_secs / FRAME_SECS)
    pause = int(min_pause_secs / FRAME_SECS)
    pad = int(pad_secs / FRAME_SECS)

    # Runs of voiced frames, joined across pauses shorter than `pause`.
    runs, i, n = [], 0, len(voiced)
    while i < n:
        if not voiced[i]:
            i += 1
            continue
        start = end = i
        silent = 0
        j = i
        while j < n and silent < pause:
            if voiced[j]:
                end, silent = j + 1, 0
            else:
                silent += 1
            j += 1
        runs.append((start, end))
        i = max(j, end)

    pieces = []
    for start, end in runs:
        if (end - start) * FRAME_SECS < min_speech_secs:
            continue
        start, end = max(0, start - pad), min(n, end + pad)
        for a, b in _split_long(start, end, energy, max_frames):
            pieces.append(Piece(a * frame, min(b * frame, len(samples))))
    return pieces


def _split_long(start, end, energy, max_frames):
    """Cuts [start, end) into parts of at most max_frames, each cut at the
    quietest frame in the back half of the allowed span."""
    out = []
    while end - start > max_frames:
        lo, hi = start + max_frames // 2, start + max_frames
        cut = lo + int(np.argmin(energy[lo:hi]))
        out.append((start, cut))
        start = cut
    out.append((start, end))
    return out
