"""One dictation: a short Ogg Opus clip in, text out. Runs in the request's
thread, ahead of any meeting transcription (see EngineCache.dictation)."""
import time

from . import audio, chunking


class NoModel(Exception):
    """No model has been chosen yet."""


class Loading(Exception):
    """The chosen model is still loading."""


class ModelFailed(Exception):
    """The chosen model could not be loaded."""


class BadAudio(Exception):
    """The clip could not be decoded."""


def transcribe(engines, data):
    """-> {"text", "model", "secs"}. The text is empty when the clip holds no speech."""
    status = engines.status()
    if status["active"] is None:
        raise NoModel("no model selected; load one first")
    if status["loaded"] != status["active"]:
        if status["error"]:
            raise ModelFailed(f"the {status['active']} model failed to load: {status['error']}")
        raise Loading(f"the {status['active']} model is still loading")
    t0 = time.perf_counter()
    try:
        samples = audio.decode_bytes(data)
    except Exception as e:
        raise BadAudio(f"could not read the audio: {e}") from e
    # Shorter speech than a meeting line: a single word is a dictation.
    pieces = chunking.plan(samples, min_speech_secs=0.15)
    if not pieces:
        return {"text": "", "model": status["loaded"], "secs": time.perf_counter() - t0}
    with engines.dictation():
        engine = engines.get()
        texts = engine.transcribe(samples, pieces)
    text = " ".join(t.strip() for t in texts if t.strip())
    return {"text": text, "model": engine.name, "secs": time.perf_counter() - t0}
