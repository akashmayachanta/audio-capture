"""Unpacks Underdog Bark 3.0's `model.bark` into a stock transformers ParakeetForTDT.

Adapted from `bark_loader.py` in ConwayResearch/Underdog-Bark-3.0 (CC-BY-4.0,
see BARK_NOTICE): vendored instead of downloaded so no fetched code runs, and
the NVIDIA processor/generation configs come from a pinned revision into our
cache. The unpacking itself is unchanged.

Formats "bark-levels-v2" and "bark-levels-v3" (v3 = v2 with LZMA on the small side blobs):
  encoder linear weights: per-row levels (fp16) + one code per weight, entropy coded with ANS;
  other matrices: per-row symmetric integers (5 or 6 bits), ANS coded, with fp16 row scales;
  small tensors: fp16.
"""
import json
import lzma
import os
from pathlib import Path

import numpy as np

BASE_REPO = "nvidia/parakeet-tdt-0.6b-v3"
BASE_REVISION = "541d1f99c6b0c3cd0b11a95167540bb8edefd82b"
NC = 16


def _decode_codes(cls, counts, words, rows, cols):
    import constriction
    dec = constriction.stream.stack.AnsCoder(words)
    codes = np.empty((rows, cols), np.uint8)
    for c in range(len(counts)):
        idx = np.nonzero(cls == c)[0]
        model = constriction.stream.model.Categorical(counts[c] / counts[c].sum(), perfect=False)
        codes[:, idx] = dec.decode(model, rows * len(idx)).reshape(len(idx), rows).T
    return codes


def _levels_to_weights(lev, codes):
    K = lev.shape[1]
    lev = lev.astype(np.float32)
    mag = np.where(codes == 0, 0, np.where(codes <= K, codes, codes - K)).astype(np.int64)
    val = np.take_along_axis(np.concatenate([np.zeros((lev.shape[0], 1), np.float32), lev], 1), mag, 1)
    return np.where(codes > K, -val, val)


def _unz(b, flag):
    return lzma.decompress(b) if flag else b


def load_state_dict(path):
    """-> (state_dict of numpy arrays, model config dict)."""
    import constriction
    with open(path, "rb") as f:
        raw = f.read()
    n = int.from_bytes(raw[:8], "little")
    head = json.loads(raw[8:8 + n])
    if head["format"] not in ("bark-levels-v2", "bark-levels-v3"):
        raise ValueError(f"unsupported .bark format {head['format']!r}")
    p = 8 + n
    sd = {}
    for e in head["index"]:
        shape = e["shape"]
        if e["k"] == "levels2":
            K, rows, cols = e["K"], shape[0], int(np.prod(shape[1:]))
            lev = np.frombuffer(_unz(raw[p:p + e["lev"]], e.get("zl")), np.float16).reshape(rows, K); p += e["lev"]
            meta = raw[p:p + e["meta"]]; p += e["meta"]
            nb = (cols * 4 + 7) // 8
            bits = np.unpackbits(np.frombuffer(meta[:nb], np.uint8))[:cols * 4].reshape(cols, 4)
            cls = np.packbits(np.concatenate([np.zeros((cols, 4), np.uint8), bits], 1), axis=1)[:, 0]
            counts = np.frombuffer(meta[nb:], np.uint32).reshape(NC, 2 * K + 1).astype(np.float64)
            words = np.frombuffer(raw[p:p + e["b"]], np.uint32).copy(); p += e["b"]
            codes = _decode_codes(cls, counts, words, rows, cols)
            sd[e["n"]] = _levels_to_weights(lev, codes).reshape(shape)
        elif e["k"].startswith("ansint"):
            bits = int(e["k"][6:])
            scale = np.frombuffer(_unz(raw[p:p + e["s"]], e.get("zs")), np.float16).astype(np.float32); p += e["s"]
            counts = np.frombuffer(raw[p:p + e["meta"]], np.uint32).astype(np.float64); p += e["meta"]
            words = np.frombuffer(raw[p:p + e["b"]], np.uint32).copy(); p += e["b"]
            model = constriction.stream.model.Categorical(counts / counts.sum(), perfect=False)
            sym = constriction.stream.stack.AnsCoder(words).decode(model, int(np.prod(shape)))
            q = (sym - (2 ** (bits - 1) - 1)).astype(np.float32)
            sd[e["n"]] = (q.reshape(len(scale), -1) * scale[:, None]).reshape(shape)
        elif e["k"] == "raw":
            dtype = {"torch.int64": np.int64, "torch.int32": np.int32}.get(e["dtype"], np.float16)
            sd[e["n"]] = np.frombuffer(_unz(raw[p:p + e["b"]], e.get("zr")), dtype).copy().reshape(shape); p += e["b"]
        else:
            raise ValueError(e["k"])
    return sd, json.loads(head["config"])


def cached_state_dict(path, rev, encoder=True):
    """`load_state_dict(path)`, unpacked once: the weights are kept next to
    `path` as fp16 safetensors (the 45 s ANS decode becomes a 5 s read; fp16
    gave word-for-word the same transcripts). `rev` names the files, so a new
    model revision is unpacked again. `encoder=False` leaves out the encoder's
    weights (most of the file) when they are already cached."""
    from safetensors import safe_open
    from safetensors.numpy import save_file
    folder = Path(path).parent
    weights = folder / f"unpacked-{rev[:8]}.safetensors"
    config = folder / f"config-{rev[:8]}.json"
    if weights.exists() and config.exists():
        with safe_open(str(weights), "np") as f:
            sd = {k: f.get_tensor(k) for k in f.keys() if encoder or not k.startswith("encoder.")}
        return sd, json.loads(config.read_text())
    sd, cfg = load_state_dict(path)
    sd = {k: v.astype(np.float16) if v.dtype.kind == "f" else v for k, v in sd.items()}
    tmp = weights.with_name(weights.name + ".tmp")
    save_file(sd, str(tmp))
    os.replace(tmp, weights)
    tmp = config.with_name(config.name + ".tmp")
    tmp.write_text(json.dumps(cfg))
    os.replace(tmp, config)  # last: both files exist = the cache is complete
    return sd, cfg


def _pretrained(cls, cache_dir):
    """From our cache without asking the network; downloads the first time."""
    try:
        return cls.from_pretrained(BASE_REPO, revision=BASE_REVISION, cache_dir=cache_dir, local_files_only=True)
    except OSError:
        return cls.from_pretrained(BASE_REPO, revision=BASE_REVISION, cache_dir=cache_dir)


def build_model(sd, cfg, cache_dir, skip_encoder=False):
    """-> (ParakeetForTDT in eval mode, its processor). With `skip_encoder` the
    encoder's weights stay unset: something else runs that part."""
    import torch
    from transformers import AutoProcessor, GenerationConfig, ParakeetForTDT, ParakeetTDTConfig
    from transformers.initialization import no_init_weights
    with no_init_weights():  # every weight is loaded below; random init of 600M of them took 19 s
        model = ParakeetForTDT(ParakeetTDTConfig.from_dict(cfg))
    tensors = {k: torch.from_numpy(v).float() for k, v in sd.items()
               if not (skip_encoder and k.startswith("encoder."))}
    missing, unexpected = model.load_state_dict(tensors, strict=False)
    if unexpected or not all("num_batches_tracked" in m or (skip_encoder and m.startswith("encoder."))
                             for m in missing):
        raise RuntimeError(f"model.bark does not match ParakeetForTDT: missing {missing[:5]}, unexpected {unexpected[:5]}")
    model.generation_config = _pretrained(GenerationConfig, cache_dir)
    return model.eval(), _pretrained(AutoProcessor, cache_dir)


def load_model(path, cache_dir, device="cpu", rev=""):
    """The whole model in PyTorch."""
    import torch
    model, processor = build_model(*cached_state_dict(path, rev), cache_dir)
    return model.to(device=device, dtype=torch.float32), processor


def _openvino():
    """OpenVINO with its usage telemetry off: its converter reports to Intel
    through `openvino_telemetry` and uses a stub when that module is missing."""
    import sys
    sys.modules.setdefault("openvino_telemetry", None)
    import openvino
    return openvino


def openvino_gpu():
    """Is there a GPU OpenVINO can run on (Intel graphics)?"""
    try:
        return "GPU" in _openvino().Core().available_devices
    except Exception:
        return False


def export_encoder(model, processor, xml_path):
    """Writes the encoder as OpenVINO IR (fp16 weights) at `xml_path`."""
    import torch
    openvino = _openvino()

    class Encoder(torch.nn.Module):
        def __init__(self, encoder):
            super().__init__()
            self.encoder = encoder

        def forward(self, input_features, attention_mask):
            out = self.encoder(input_features=input_features, attention_mask=attention_mask,
                               output_attention_mask=True)
            return out.last_hidden_state, out.attention_mask

    # Two rows of different lengths, so batch and time are traced as dynamic.
    waves = [np.zeros(32000, np.float32), np.zeros(16000, np.float32)]
    example = processor(waves, sampling_rate=16000, return_tensors="pt", padding=True)
    with torch.inference_mode():
        ir = openvino.convert_model(Encoder(model.encoder),
                                    example_input=(example["input_features"], example["attention_mask"]))
    xml_path = Path(xml_path)
    tmp = xml_path.with_name("tmp-" + xml_path.name)
    openvino.save_model(ir, str(tmp), compress_to_fp16=True)
    os.replace(tmp.with_suffix(".bin"), xml_path.with_suffix(".bin"))
    os.replace(tmp, xml_path)  # last: the .xml exists = the export is complete


def load_openvino(path, cache_root, rev, ov_device="GPU"):
    """-> (model, processor) with the encoder (85% of the work) compiled by
    OpenVINO for `ov_device`; decoder and joint stay in PyTorch on the CPU.
    The first call converts the encoder (about a minute) and compiles it
    (another); both are kept, in the model folder and `ov_cache`."""
    import torch
    openvino = _openvino()
    from transformers.models.parakeet.modeling_parakeet import ParakeetEncoderModelOutput
    xml = Path(path).parent / f"encoder-{rev[:8]}.xml"
    exported = xml.exists()
    sd, cfg = cached_state_dict(path, rev, encoder=not exported)
    model, processor = build_model(sd, cfg, str(Path(cache_root) / "hf"), skip_encoder=exported)
    del sd
    if not exported:
        export_encoder(model, processor, xml)
    core = openvino.Core()
    core.set_property({"CACHE_DIR": str(Path(cache_root) / "ov_cache")})
    compiled = core.compile_model(core.read_model(str(xml)), ov_device, {"PERFORMANCE_HINT": "LATENCY"})

    def forward(input_features=None, attention_mask=None, **_):
        out = compiled([input_features.numpy(), attention_mask.numpy()])
        hidden, mask = out[0], out[1]
        return ParakeetEncoderModelOutput(last_hidden_state=torch.from_numpy(hidden.copy()),
                                          attention_mask=torch.from_numpy(mask.copy()).int())

    # Free the PyTorch encoder's weights (2.4 GB as float32); only its forward is used.
    for p in model.encoder.parameters():
        p.data = torch.empty(0)
    model.encoder.forward = forward
    return model, processor
