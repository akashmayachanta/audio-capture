"""Decodes a MeetRec recording into one 16 kHz mono float32 array per speaker.

Stereo files carry the mic on the left and system audio on the right; mono
files carry whichever the sidecar's channel map names ("mono": "mic" | "system").
"""
import numpy as np

SAMPLE_RATE = 16000
SPEAKER_OF_SOURCE = {"mic": "me", "system": "others"}


def speakers_for(channels, n_channels):
    """Channel map from the sidecar -> speaker per decoded channel."""
    if n_channels == 1:
        return [SPEAKER_OF_SOURCE.get((channels or {}).get("mono", "mic"), "me")]
    channels = channels or {"left": "mic", "right": "system"}
    return [SPEAKER_OF_SOURCE.get(channels.get(side, default), "me")
            for side, default in (("left", "mic"), ("right", "system"))][:n_channels]


def decode_bytes(data):
    """A dictation clip (Ogg Opus bytes) -> 16 kHz mono float32; the first channel if there are more."""
    import io

    import av
    with av.open(io.BytesIO(data)) as container:
        stream = container.streams.audio[0]
        resampler = av.AudioResampler(format="fltp", layout=stream.layout.name, rate=SAMPLE_RATE)
        parts = []
        for frame in container.decode(stream):
            frame.pts = None
            parts.extend(r.to_ndarray() for r in resampler.resample(frame))
        parts.extend(r.to_ndarray() for r in resampler.resample(None))
    if not parts:
        return np.zeros(0, np.float32)
    return np.ascontiguousarray(np.concatenate(parts, axis=1)[0], dtype=np.float32)


def decode(path, channels=None):
    """-> {"me": samples, "others": samples} (only the speakers the file holds)."""
    import av
    with av.open(str(path)) as container:
        stream = container.streams.audio[0]
        n = stream.codec_context.channels
        resampler = av.AudioResampler(format="fltp", layout=stream.layout.name, rate=SAMPLE_RATE)
        parts = []
        for frame in container.decode(stream):
            frame.pts = None
            parts.extend(r.to_ndarray() for r in resampler.resample(frame))
        parts.extend(r.to_ndarray() for r in resampler.resample(None))
    audio = np.concatenate(parts, axis=1) if parts else np.zeros((n, 0), np.float32)
    return {speaker: np.ascontiguousarray(audio[i], dtype=np.float32)
            for i, speaker in enumerate(speakers_for(channels, n))}
