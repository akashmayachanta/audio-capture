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


def load_model(path, cache_dir, device="cpu"):
    """-> (ParakeetForTDT in eval mode, its processor)."""
    import torch
    from transformers import AutoProcessor, GenerationConfig, ParakeetForTDT, ParakeetTDTConfig
    sd, cfg = load_state_dict(path)
    model = ParakeetForTDT(ParakeetTDTConfig.from_dict(cfg))
    missing, unexpected = model.load_state_dict({k: torch.from_numpy(v).float() for k, v in sd.items()}, strict=False)
    if unexpected or not all("num_batches_tracked" in m for m in missing):
        raise RuntimeError(f"model.bark does not match ParakeetForTDT: missing {missing[:5]}, unexpected {unexpected[:5]}")
    model.generation_config = GenerationConfig.from_pretrained(BASE_REPO, revision=BASE_REVISION, cache_dir=cache_dir)
    processor = AutoProcessor.from_pretrained(BASE_REPO, revision=BASE_REVISION, cache_dir=cache_dir)
    return model.eval().to(device=device, dtype=torch.float32), processor
