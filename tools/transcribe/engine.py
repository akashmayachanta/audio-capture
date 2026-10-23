"""OpenVINO side of the Parakeet TDT pipeline: model loading with per-component
device placement, and the mel -> encoder -> decoder/joint calls for one window."""
import os
import sys
import time

import numpy as np

import pipeline as p

MODEL_FILES = {
    "mel": "parakeet_melspectogram.xml",  # sic: spelled this way upstream
    "encoder": "parakeet_encoder.xml",
    "decoder": "parakeet_decoder.xml",
    "joint": "parakeet_joint.xml",
}
VOCAB_FILE = "parakeet_v3_vocab.json"
REQUIRED_FILES = [VOCAB_FILE] + [f for xml in MODEL_FILES.values() for f in (xml, xml[:-4] + ".bin")]
SILENCE_PEAK = 0.002  # below this a window holds no speech; skip it


def log(msg):
    print(msg, file=sys.stderr, flush=True)


def device_plan(device):
    """Device per component. The mel graph only runs on CPU. For `auto` every
    component tries the NPU first (measured on a Core Ultra 7 155U: all of them
    compile there and the whole pipeline is faster than with decoder/joint on
    CPU); a component the NPU compiler rejects falls back to the next device. A
    named device is used for all three, again falling back to CPU."""
    device = device.upper()
    if device == "AUTO":
        return {"mel": "CPU", "encoder": "NPU,GPU,CPU", "decoder": "NPU,CPU", "joint": "NPU,CPU"}
    return {"mel": "CPU", "encoder": device, "decoder": device, "joint": device}


class Engine:
    def __init__(self, model_dir, ov_cache_dir, plan):
        import openvino as ov
        self.core = ov.Core()
        self.ov_cache_dir = ov_cache_dir
        os.makedirs(ov_cache_dir, exist_ok=True)
        self.core.set_property({"CACHE_DIR": ov_cache_dir})
        self.vocab = p.load_vocab(os.path.join(model_dir, VOCAB_FILE))
        self.placement, self.compile_secs = {}, {}
        self.times = {"mel": 0.0, "encoder": 0.0, "decoder": 0.0, "joint": 0.0}
        self.calls = {"decoder": 0, "joint": 0}
        self.models = {}
        for name, xml in MODEL_FILES.items():
            self.models[name] = self._compile(name, os.path.join(model_dir, xml), plan[name])
        self.requests = {name: m.create_infer_request() for name, m in self.models.items()}
        self.mel_samples = self.models["mel"].input(0).get_shape()[1]
        if self.mel_samples != p.WINDOW:
            raise RuntimeError(f"mel graph window is {self.mel_samples} samples, expected {p.WINDOW}")

    def _compile(self, name, path, spec):
        errors = []
        for device in [d.strip() for d in spec.split(",")] + ["CPU"]:
            if device not in self.core.available_devices:
                errors.append(f"{device}: not available")
                continue
            t0 = time.perf_counter()
            try:
                model = self.core.compile_model(path, device)
            except Exception as e:  # compiler rejections surface as RuntimeError
                errors.append(f"{device}: {str(e).splitlines()[0][:200]}")
                log(f"{name}: compile on {device} failed, trying next ({errors[-1]})")
                continue
            self.compile_secs[name] = time.perf_counter() - t0
            self.placement[name] = device
            log(f"{name}: {device} compiled in {self.compile_secs[name]:.2f}s")
            return model
        raise RuntimeError(f"cannot compile {name}: " + "; ".join(errors))

    # -- per-window inference -------------------------------------------------

    def encode(self, samples):
        """float32 mono samples (<= WINDOW) -> (frames [T, 1024], valid frames)."""
        n = len(samples)
        padded = min(-(-n // 1000) * 1000, p.WINDOW)  # the mel graph chokes on odd lengths
        buf = np.zeros((1, p.WINDOW), dtype=np.float32)
        buf[0, :n] = samples
        t0 = time.perf_counter()
        req = self.requests["mel"]
        req.infer([buf, np.array([padded], dtype=np.int64)])
        mel = np.array(req.get_output_tensor(0).data)
        mel_len = int(req.get_output_tensor(1).data[0])
        t1 = time.perf_counter()
        req = self.requests["encoder"]
        req.infer([mel, np.array([min(mel_len, mel.shape[2])], dtype=np.int32)])
        enc = np.array(req.get_output_tensor(0).data)  # [1, 1024, T]
        valid = min(int(req.get_output_tensor(1).data[0]), enc.shape[2])
        t2 = time.perf_counter()
        self.times["mel"] += t1 - t0
        self.times["encoder"] += t2 - t1
        return np.ascontiguousarray(enc[0].T)[:, None, None, :], valid

    def initial_state(self):
        zeros = np.zeros((2, 1, 640), dtype=np.float32)
        return zeros, zeros.copy()

    def predictor(self, token, state):
        h, c = state
        req = self.requests["decoder"]
        t0 = time.perf_counter()
        req.infer([np.array([[token]], dtype=np.int64), h, c])
        dec_out = np.array(req.get_output_tensor(0).data)
        new_state = (np.array(req.get_output_tensor(1).data), np.array(req.get_output_tensor(2).data))
        self.times["decoder"] += time.perf_counter() - t0
        self.calls["decoder"] += 1
        return dec_out, new_state

    def joint_for(self, frames):
        req = self.requests["joint"]

        def joint(t, dec_out):
            t0 = time.perf_counter()
            req.infer([frames[t], dec_out])
            logits = req.get_output_tensor(0).data.reshape(-1)
            self.times["joint"] += time.perf_counter() - t0
            self.calls["joint"] += 1
            return logits
        return joint

    def decode_window(self, samples):
        """-> [(token_id, frame)] for one window of audio, or [] if it is silent."""
        if len(samples) == 0 or float(np.abs(samples).max()) < SILENCE_PEAK:
            return []
        frames, valid = self.encode(samples)
        if valid == 0:
            return []
        return p.tdt_greedy(valid, self.predictor, self.joint_for(frames), self.initial_state())


def transcribe_samples(engine, samples, on_chunk=None):
    """float32 mono 16 kHz samples -> [Word] for the whole channel."""
    chunks = p.plan_chunks(len(samples))
    per_chunk = []
    for i, chunk in enumerate(chunks):
        pieces = engine.decode_window(samples[chunk.start:chunk.end])
        base = chunk.start / p.SAMPLE_RATE
        per_chunk.append(p.words_from_pieces([(tok, base + f * p.FRAME_SECS) for tok, f in pieces], engine.vocab))
        if on_chunk:
            on_chunk(i + 1, len(chunks))
    return p.merge_chunk_words(chunks, per_chunk)
