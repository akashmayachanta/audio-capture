"""One transcription at a time, on a worker thread; the page polls `snapshot()`."""
import itertools
import logging
import queue
import threading
import time
import traceback
from dataclasses import dataclass, field, fields
from pathlib import Path

from . import audio, chunking, markdown
from .engines import Cancelled
from .recordings import app_of, read_sidecar, started

log = logging.getLogger("transcriber")
_ids = itertools.count(1)


@dataclass
class Job:
    rec_id: str
    path: Path
    model: str = ""  # set when the job starts: the model loaded then
    id: int = field(default_factory=lambda: next(_ids))
    state: str = "queued"  # queued | running | done | error | cancelled
    phase: str = "Waiting"
    progress: float = 0.0
    error: str = ""
    created: float = field(default_factory=time.time)
    finished: float | None = None
    cancel: threading.Event = field(default_factory=threading.Event, repr=False)

    def public(self):
        return {f.name: getattr(self, f.name) for f in fields(self) if f.name not in ("cancel", "path")}


class Jobs:
    def __init__(self, engines, transcribe_fn=None):
        self.engines = engines
        self.transcribe_fn = transcribe_fn or transcribe_recording
        self.lock = threading.Lock()
        self.jobs = []  # newest last; finished ones are kept for the page to show
        self.queue = queue.Queue()
        threading.Thread(target=self._worker, name="transcribe", daemon=True).start()

    def submit(self, rec_id, path):
        with self.lock:
            for j in self.jobs:
                if j.rec_id == rec_id and j.state in ("queued", "running"):
                    return j
            job = Job(rec_id, path)
            self.jobs.append(job)
            self.jobs = self.jobs[-50:]
        self.queue.put(job)
        return job

    def cancel(self, job_id):
        with self.lock:
            for j in self.jobs:
                if j.id == job_id and j.state in ("queued", "running"):
                    j.cancel.set()
                    if j.state == "queued":
                        j.state, j.phase, j.finished = "cancelled", "Cancelled", time.time()
                    return True
        return False

    def snapshot(self):
        with self.lock:
            return [j.public() for j in self.jobs]

    def _worker(self):
        while True:
            job = self.queue.get()
            if job.state != "queued":
                continue
            job.state = "running"
            try:
                self.transcribe_fn(job, self.engines)
                job.state, job.phase, job.progress = "done", "Done", 1.0
            except Cancelled:
                job.state, job.phase = "cancelled", "Cancelled"
            except Exception as e:
                log.error("transcribing %s failed:\n%s", job.rec_id, traceback.format_exc())
                job.state, job.phase, job.error = "error", "Failed", f"{type(e).__name__}: {e}"
            job.finished = time.time()


def transcribe_recording(job, engines):
    """Decode -> cut at pauses -> model -> `<name>.md`. Progress is by audio
    seconds, so a long piece moves the bar more than a short one."""
    status = engines.status()
    if status["loaded"] != status["active"]:
        job.phase = "Waiting for the model to load"
    engine = engines.get()
    job.model = engine.id
    if job.cancel.is_set():
        raise Cancelled()

    job.phase = "Decoding audio"
    meta = read_sidecar(job.path)
    channels = audio.decode(job.path, meta.get("channels"))
    duration = meta.get("duration_secs") or max((len(x) for x in channels.values()), default=0) / audio.SAMPLE_RATE

    plans = {spk: chunking.plan(x) for spk, x in channels.items()}
    total = sum(p.end - p.start for ps in plans.values() for p in ps) or 1
    done = 0
    t0 = time.perf_counter()
    per_channel = []
    for spk, pieces in plans.items():
        job.phase = f"Transcribing {markdown.LABELS[spk]}"

        def on_piece(i, pieces=pieces):
            nonlocal done
            done += pieces[i].end - pieces[i].start
            job.progress = done / total
        texts = engine.transcribe(channels[spk], pieces, on_piece, job.cancel.is_set, turn=engines.turn)
        per_channel.append([
            markdown.Segment(spk, *p.secs(audio.SAMPLE_RATE), text)
            for p, text in zip(pieces, texts) if text.strip()
        ])
    secs = time.perf_counter() - t0

    when = started(meta, job.path)
    app = app_of(meta, job.path)
    t = markdown.Transcript(
        audio_file=job.path.name,
        title=(when.strftime("%Y-%m-%d %H:%M") + (f" · {app}" if app else "")) if when else job.path.stem,
        started=when.strftime("%Y-%m-%d %H:%M") if when else "",
        apps=meta.get("apps") or [],
        duration_secs=duration,
        model=engine.name,
        model_credit=engine.credit,
        device=engine.device_label,
        rtfx=duration / secs if secs > 0 else None,
        segments=markdown.merge(*per_channel),
    )
    job.phase = "Writing transcript"
    markdown.write(str(job.path), t)
