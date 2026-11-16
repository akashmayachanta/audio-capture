"""The two speech models behind one interface: `transcribe(samples, pieces, ...)`
returns one text per piece and calls `on_piece(index)` as each one finishes
(Bark batches by length, so out of order). Weights are downloaded once, at
pinned revisions, into the cache folder."""
import logging
import os
import threading
import time

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
    """Underdog Bark 3.0: Parakeet TDT 0.6B v3 fine-tune, English, PyTorch."""
    id = "bark"
    name = "underdog-bark-3.0"
    credit = ("Underdog Bark 3.0 by Conway Research (CC-BY-4.0), "
              "fine-tuned from NVIDIA Parakeet TDT 0.6B v3")
    REPO = "ConwayResearch/Underdog-Bark-3.0"
    REVISION = "85db2ad18ab41e534a222ee253c29191d77ed919"
    LEAD_IN_SECS = 0.25  # silence before each piece, as the model was evaluated
    BATCH = 4

    def __init__(self, cache_root, device="cpu"):
        import torch
        from . import bark_loader
        if device == "cuda" and not torch.cuda.is_available():
            raise RuntimeError("device = \"cuda\" but this torch build has no CUDA; use \"cpu\"")
        self.torch = torch
        self.device = device
        path = _download(self.REPO, self.REVISION, "model.bark", cache_root / "models" / "underdog-bark-3.0")
        self.model, self.processor = bark_loader.load_model(path, cache_dir=str(cache_root / "hf"), device=device)
        self.device_label = device.upper() if device == "cuda" else f"CPU ({torch.get_num_threads()} threads)"

    def transcribe(self, samples, pieces, on_piece=lambda i: None, cancelled=lambda: False):
        lead = np.zeros(int(self.LEAD_IN_SECS * SAMPLE_RATE), np.float32)
        texts = [""] * len(pieces)
        # Similar lengths per batch keep the padding small.
        order = sorted(range(len(pieces)), key=lambda i: pieces[i].end - pieces[i].start)
        for b in range(0, len(order), self.BATCH):
            if cancelled():
                raise Cancelled()
            idx = order[b:b + self.BATCH]
            waves = [np.concatenate([lead, samples[pieces[i].start:pieces[i].end]]) for i in idx]
            inputs = self.processor(waves, sampling_rate=SAMPLE_RATE, return_tensors="pt", padding=True)
            inputs = {k: v.to(self.device) for k, v in inputs.items()}
            with self.torch.inference_mode():
                out = self.model.generate(**inputs)
            for i, text in zip(idx, self.processor.batch_decode(out.sequences, skip_special_tokens=True)):
                texts[i] = text.strip()
                on_piece(i)
        return texts


class WhistleEngine:
    """Cactus Whistle: 17 MB, 7 languages, native CPU engine (cactus-needle)."""
    id = "whistle"
    name = "cactus-whistle"
    credit = "Cactus Whistle by Cactus Compute (Apache-2.0)"
    REPO = "Cactus-Compute/whistle"
    REVISION = "741118687e1f008f06d1351a402a731274789790"

    def __init__(self, cache_root, language="en"):
        from needle.agent.whistle import LANGUAGES, Whistle
        if language not in ("auto",) + LANGUAGES:
            raise RuntimeError(f"language {language!r} is not one of auto, {', '.join(LANGUAGES)}")
        self.language = None if language == "auto" else language
        path = _download(self.REPO, self.REVISION, "whistle.cact", cache_root / "models" / "whistle")
        self.model = Whistle(weights=path)
        self.device_label = "CPU"

    def transcribe(self, samples, pieces, on_piece=lambda i: None, cancelled=lambda: False):
        texts = []
        for i, p in enumerate(pieces):
            if cancelled():
                raise Cancelled()
            result = self.model.transcribe(samples[p.start:p.end], language=self.language)
            texts.append(result.get("text", "").strip())
            on_piece(i)
        return texts


CATALOG = [  # in display order
    {"id": "whistle", "label": "Cactus Whistle",
     "description": "English, German, French, Spanish, Italian, Dutch, Polish. Fast and small (17 MB), less accurate."},
    {"id": "bark", "label": "Underdog Bark 3.0",
     "description": "English. Most accurate. 163 MB download, PyTorch; about a minute to load."},
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
        key = (model, s.device if model == "bark" else s.language)
        with self.lock:
            if self.key == key:
                return self.engine
            self.engine, self.key = None, None  # free the old one before loading the next
            self.loading, self.error = model, ""
            t0 = time.perf_counter()
            try:
                if model == "bark":
                    self.engine = BarkEngine(s.cache_root(), s.device)
                else:
                    self.engine = WhistleEngine(s.cache_root(), s.language)
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

    def status(self):
        return {
            "active": self.active,
            "loaded": self.key[0] if self.key else None,
            "loading": self.loading,
            "error": self.error,
            "load_secs": self.load_secs,
            "device": self.engine.device_label if self.engine else "",
        }
