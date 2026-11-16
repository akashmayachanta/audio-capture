"""The two speech models behind one interface: `transcribe(samples, pieces, ...)`
returns one text per piece and calls `on_piece(index)` as each one finishes
(Bark batches by length, so out of order). Weights are downloaded once, at
pinned revisions, into the cache folder."""
import logging
import os
import threading
import time
from contextlib import contextmanager, nullcontext

import numpy as np

from .audio import SAMPLE_RATE

log = logging.getLogger("transcriber")
os.environ.setdefault("NEEDLE_TELEMETRY", "0")  # cactus-needle phones home unless told not to


class Cancelled(Exception):
    pass


def _download(repo, revision, filename, dest):
    from huggingface_hub import hf_hub_download
    return hf_hub_download(repo, filename, revision=revision, local_dir=dest)


class BarkEngine:
    """Underdog Bark 3.0: Parakeet TDT 0.6B v3 fine-tune, English. PyTorch,
    with the encoder optionally on Intel graphics through OpenVINO."""
    id = "bark"
    name = "underdog-bark-3.0"
    credit = ("Underdog Bark 3.0 by Conway Research (CC-BY-4.0), "
              "fine-tuned from NVIDIA Parakeet TDT 0.6B v3")
    REPO = "ConwayResearch/Underdog-Bark-3.0"
    REVISION = "85db2ad18ab41e534a222ee253c29191d77ed919"
    LEAD_IN_SECS = 0.25  # silence before each piece, as the model was evaluated
    BATCH = 4
    FRAME_BUCKET = 256  # OpenVINO: input lengths are padded up to a multiple of this (2.56 s)

    def __init__(self, cache_root, device="auto"):
        """`device`: "cpu" / "cuda" = all of it in PyTorch; "gpu" = the encoder
        on Intel graphics through OpenVINO (measured on a Core Ultra 7 155U:
        11-14x real time against 4x on the CPU); "auto" = "gpu" when there is
        one and it loads, else "cpu"."""
        import torch
        from . import bark_loader
        if device not in ("auto", "cpu", "gpu", "cuda"):
            raise RuntimeError(f"device {device!r} is not one of auto, cpu, gpu, cuda")
        if device == "cuda" and not torch.cuda.is_available():
            raise RuntimeError("device = \"cuda\" but this torch build has no CUDA; use \"cpu\"")
        self.torch = torch
        path = _download(self.REPO, self.REVISION, "model.bark", cache_root / "models" / "underdog-bark-3.0")
        self.openvino = False
        if device == "gpu" or (device == "auto" and bark_loader.openvino_gpu()):
            try:
                self.model, self.processor = bark_loader.load_openvino(path, cache_root, self.REVISION)
                self.openvino = True
            except Exception as e:
                if device == "gpu":
                    raise RuntimeError(f"device = \"gpu\": OpenVINO could not load Bark on the GPU ({e})") from e
                log.warning("Bark on the GPU failed, using the CPU: %s", e)
        self.device = "cuda" if device == "cuda" else "cpu"  # where the PyTorch part runs
        if not self.openvino:
            self.model, self.processor = bark_loader.load_model(
                path, cache_dir=str(cache_root / "hf"), device=self.device, rev=self.REVISION)
        self.device_label = ("Intel GPU (OpenVINO)" if self.openvino else "CUDA" if device == "cuda"
                             else f"CPU ({torch.get_num_threads()} threads)")

    def _fixed_shape(self, inputs):
        """The GPU compiles kernels for every new input shape (about 8 s each,
        then cached), so pad the length to a few fixed sizes. The attention
        mask marks the padding."""
        pad = -inputs["input_features"].shape[1] % self.FRAME_BUCKET
        F = self.torch.nn.functional
        return {"input_features": F.pad(inputs["input_features"], (0, 0, 0, pad)),
                "attention_mask": F.pad(inputs["attention_mask"], (0, pad))}

    def transcribe(self, samples, pieces, on_piece=lambda i: None, cancelled=lambda: False, turn=nullcontext):
        """`turn()` is a context manager held around each batch, so a dictation can cut in between batches."""
        lead = np.zeros(int(self.LEAD_IN_SECS * SAMPLE_RATE), np.float32)
        texts = [""] * len(pieces)
        # Similar lengths per batch keep the padding small.
        order = sorted(range(len(pieces)), key=lambda i: pieces[i].end - pieces[i].start)
        for b in range(0, len(order), self.BATCH):
            if cancelled():
                raise Cancelled()
            idx = order[b:b + self.BATCH]
            waves = [np.concatenate([lead, samples[pieces[i].start:pieces[i].end]]) for i in idx]
            if self.openvino:
                waves += [waves[-1]] * (self.BATCH - len(waves))  # always a full batch: one shape fewer
            inputs = self.processor(waves, sampling_rate=SAMPLE_RATE, return_tensors="pt", padding=True)
            inputs = self._fixed_shape(inputs) if self.openvino else {k: v.to(self.device) for k, v in inputs.items()}
            with turn(), self.torch.inference_mode():
                out = self.model.generate(**inputs)
            for i, text in zip(idx, self.processor.batch_decode(out.sequences, skip_special_tokens=True)):
                texts[i] = text.strip()
                on_piece(i)
        return texts


_worker_model = None  # the Whistle in a pool worker process


def _whistle_worker_init(path):
    global _worker_model
    os.environ["NEEDLE_TELEMETRY"] = "0"
    from needle.agent.whistle import Whistle
    _worker_model = Whistle(weights=path)


def _whistle_worker_run(samples, language):
    return _worker_model.transcribe(samples, language=language).get("text", "").strip()


class WhistleEngine:
    """Cactus Whistle: 17 MB, 7 languages, native CPU engine (cactus-needle).

    One Whistle per process (the native engine holds one model and isn't
    thread-safe) uses only ~2 cores, so with `workers` > 1 the pieces go to a
    pool of processes, each with its own copy of the model. Measured on a
    Core Ultra 7 155U: 1 process 2.1x real time, 4 processes 7.1x."""
    id = "whistle"
    name = "cactus-whistle"
    credit = "Cactus Whistle by Cactus Compute (Apache-2.0)"
    REPO = "Cactus-Compute/whistle"
    REVISION = "741118687e1f008f06d1351a402a731274789790"

    def __init__(self, cache_root, language="en", workers=1):
        from needle.agent.whistle import LANGUAGES, Whistle
        if language not in ("auto",) + LANGUAGES:
            raise RuntimeError(f"language {language!r} is not one of auto, {', '.join(LANGUAGES)}")
        self.language = None if language == "auto" else language
        self.path = _download(self.REPO, self.REVISION, "whistle.cact", cache_root / "models" / "whistle")
        self.workers = max(1, workers)
        self.model = None
        self.pool = None
        if self.workers == 1:
            self.model = Whistle(weights=self.path)
        else:
            self._start_pool()
        self.device_label = "CPU" if self.workers == 1 else f"CPU ({self.workers} processes)"

    def _start_pool(self):
        from concurrent.futures import ProcessPoolExecutor
        self.pool = ProcessPoolExecutor(self.workers, initializer=_whistle_worker_init, initargs=(self.path,))
        # Load the model in every worker now, so the first job doesn't wait for it.
        list(self.pool.map(_whistle_worker_run, [np.zeros(SAMPLE_RATE, np.float32)] * self.workers,
                           [self.language] * self.workers))

    def close(self):
        if self.pool is not None:
            procs = list((self.pool._processes or {}).values())
            self.pool.shutdown(wait=False, cancel_futures=True)
            for p in procs:  # don't wait for a piece in progress
                p.terminate()
            self.pool = None

    def transcribe(self, samples, pieces, on_piece=lambda i: None, cancelled=lambda: False, turn=nullcontext):
        if self.model is not None:
            texts = []
            for i, p in enumerate(pieces):
                if cancelled():
                    raise Cancelled()
                with turn():  # the one native model: a dictation may cut in between pieces
                    result = self.model.transcribe(samples[p.start:p.end], language=self.language)
                texts.append(result.get("text", "").strip())
                on_piece(i)
            return texts
        return self._transcribe_pool(samples, pieces, on_piece, cancelled)

    def _transcribe_pool(self, samples, pieces, on_piece, cancelled):
        from concurrent.futures import FIRST_COMPLETED, wait
        from concurrent.futures.process import BrokenProcessPool
        if self.pool is None:  # a worker died during an earlier job
            self._start_pool()
        texts = [""] * len(pieces)
        # Submit a few ahead only, so cancelling stops quickly.
        todo = iter(range(len(pieces)))
        running = {}

        def submit_next():
            i = next(todo, None)
            if i is not None:
                p = pieces[i]
                running[self.pool.submit(_whistle_worker_run, samples[p.start:p.end], self.language)] = i

        try:
            for _ in range(self.workers * 2):
                submit_next()
            while running:
                done, _ = wait(running, timeout=0.5, return_when=FIRST_COMPLETED)
                if cancelled():
                    for f in running:
                        f.cancel()
                    raise Cancelled()
                for f in done:
                    i = running.pop(f)
                    texts[i] = f.result()
                    on_piece(i)
                    submit_next()
        except BrokenProcessPool:
            self.close()
            raise RuntimeError("a Whistle worker process stopped; try again") from None
        return texts


CATALOG = [  # in display order
    {"id": "whistle", "label": "Cactus Whistle",
     "description": "English, German, French, Spanish, Italian, Dutch, Polish. Fast and small (17 MB), less accurate."},
    {"id": "bark", "label": "Underdog Bark 3.0",
     "description": "English. Most accurate. 163 MB download; the first load takes a few minutes, "
                    "later ones under a minute. Uses Intel graphics when available."},
]


class EngineCache:
    """Holds the one loaded model. `load_async` loads on a background thread;
    jobs call `get()` for the active model and wait while it is still loading.
    Loading another model frees the previous one."""

    def __init__(self, settings, on_loaded=lambda model: None):
        self.settings = settings
        self.on_loaded = on_loaded
        self.lock = threading.Lock()  # one load at a time
        self.key, self.engine = None, None
        self.active = None  # the model chosen last; jobs use it
        self.loading = None
        self.error = ""
        self.load_secs = None
        # The loaded engine does one piece at a time. A batch job takes `turn()`
        # per piece; a dictation takes `dictation()`, which waits for the piece in
        # flight and makes batch jobs wait until it is done.
        self._use = threading.Lock()
        self._turns = threading.Condition()
        self._dictating = 0

    @contextmanager
    def turn(self):
        with self._turns:
            while self._dictating:
                self._turns.wait()
        with self._use:
            yield

    @contextmanager
    def dictation(self):
        with self._turns:
            self._dictating += 1
        try:
            with self._use:
                yield
        finally:
            with self._turns:
                self._dictating -= 1
                self._turns.notify_all()

    def load_async(self, model):
        self.active, self.error = model, ""
        threading.Thread(target=self._load_logged, args=(model,), name="load-model", daemon=True).start()

    def _load_logged(self, model):
        try:
            self.get(model)
        except Exception:
            log.exception("loading %s failed", model)

    def get(self, model=None):
        model = model or self.active
        if model is None:
            raise RuntimeError("no model selected")
        s = self.settings
        key = (model, s.device if model == "bark" else (s.language, s.whistle_workers()))
        with self.lock:
            if self.key == key:
                return self.engine
            if close := getattr(self.engine, "close", None):
                close()
            self.engine, self.key = None, None  # free the old one before loading the next
            self.loading, self.error = model, ""
            t0 = time.perf_counter()
            try:
                if model == "bark":
                    self.engine = BarkEngine(s.cache_root(), s.device)
                else:
                    self.engine = WhistleEngine(s.cache_root(), s.language, s.whistle_workers())
            except Exception as e:
                if model == self.active:
                    self.error = f"{type(e).__name__}: {e}"
                raise
            finally:
                self.loading = None
            self.key, self.load_secs = key, time.perf_counter() - t0
            engine = self.engine
        self.on_loaded(model)
        return engine

    def close(self):
        """At exit: stops Whistle's worker processes."""
        if close := getattr(self.engine, "close", None):
            close()

    def status(self):
        return {
            "active": self.active,
            "loaded": self.key[0] if self.key else None,
            "loading": self.loading,
            "error": self.error,
            "load_secs": self.load_secs,
            "device": self.engine.device_label if self.engine else "",
        }
