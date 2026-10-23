"""MeetRec transcription helper: Parakeet TDT on OpenVINO (NPU first).

    python transcribe.py --me me.wav [--others others.wav] --cache-dir <cache_root> [--device auto]

Input is 16 kHz mono 16-bit PCM WAV (MeetRec writes these into <cache>\\tmp).
stdout carries JSON lines only:
    {"progress": 0.42}
    {"result": {"me": [{"start":..,"end":..,"text":..}], "others": [...], "device": ..., "rtfx": ...}}
    {"error": "message"}
Logs go to stderr. Model (CC-BY-4.0, FluidInference/parakeet-tdt-0.6b-v3-ov,
converted from nvidia/parakeet-tdt-0.6b-v3) and compiled blobs live under --cache-dir.
"""
import argparse
import json
import os
import sys
import time
import wave

import numpy as np

import engine as eng
import pipeline as p

REPO_ID = "FluidInference/parakeet-tdt-0.6b-v3-ov"
MODEL_NAME = "parakeet-tdt-0.6b-v3-ov"
MODEL_LICENSE = "CC-BY-4.0"


def emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


# -- cache folder --------------------------------------------------------------

def read_json(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def write_json(path, obj):
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=2)
    os.replace(tmp, path)


def file_sizes(model_dir):
    return {n: os.path.getsize(os.path.join(model_dir, n)) for n in eng.REQUIRED_FILES
            if os.path.isfile(os.path.join(model_dir, n))}


def model_is_intact(model_dir):
    """All required files exist and match the sizes recorded when they were downloaded."""
    sizes = file_sizes(model_dir)
    if len(sizes) != len(eng.REQUIRED_FILES):
        return False
    manifest = read_json(os.path.join(model_dir, "manifest.json"))
    return bool(manifest) and manifest.get("files") == sizes


def ensure_model(model_dir):
    """Download the model if it is missing or does not match its manifest.
    snapshot_download skips files that are already complete."""
    if model_is_intact(model_dir):
        return
    from huggingface_hub import snapshot_download, HfApi
    os.makedirs(model_dir, exist_ok=True)
    eng.log(f"downloading {REPO_ID} (~1.2 GB) to {model_dir}")
    snapshot_download(REPO_ID, local_dir=model_dir, allow_patterns=eng.REQUIRED_FILES + ["README.md", "config.json"])
    sizes = file_sizes(model_dir)
    missing = [n for n in eng.REQUIRED_FILES if n not in sizes]
    if missing:
        raise RuntimeError(f"model download incomplete, missing: {', '.join(missing)}")
    try:
        revision = HfApi().model_info(REPO_ID).sha
    except Exception:
        revision = None
    write_json(os.path.join(model_dir, "manifest.json"),
               {"repo_id": REPO_ID, "revision": revision, "license": MODEL_LICENSE, "files": sizes})


def runtime_fingerprint(core):
    import openvino as ov
    fp = {"openvino": ov.__version__}
    if "NPU" in core.available_devices:
        try:
            fp["npu_driver"] = str(core.get_property("NPU", "NPU_DRIVER_VERSION"))
        except Exception:
            pass
    return fp


def prepare_ov_cache(ov_cache_dir):
    """Compiled blobs are only valid for the OpenVINO build and NPU driver that
    made them; wipe them when either changes."""
    import shutil
    import openvino as ov
    fp = runtime_fingerprint(ov.Core())
    manifest_path = os.path.join(ov_cache_dir, "cache_manifest.json")
    if os.path.isdir(ov_cache_dir) and read_json(manifest_path) != fp:
        eng.log(f"OpenVINO/driver changed ({fp}); clearing {ov_cache_dir}")
        shutil.rmtree(ov_cache_dir, ignore_errors=True)
    os.makedirs(ov_cache_dir, exist_ok=True)
    write_json(manifest_path, fp)


# -- audio ---------------------------------------------------------------------

def read_wav(path):
    with wave.open(path, "rb") as w:
        if w.getframerate() != p.SAMPLE_RATE or w.getnchannels() != 1 or w.getsampwidth() != 2:
            raise ValueError(f"{path}: need 16 kHz mono 16-bit PCM WAV, got "
                             f"{w.getframerate()} Hz, {w.getnchannels()} ch, {8 * w.getsampwidth()}-bit")
        raw = w.readframes(w.getnframes())
    return np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0


# -- orchestration -------------------------------------------------------------

def parse_plan(device, overrides):
    plan = eng.device_plan(device)
    for item in filter(None, (overrides or "").split(";")):
        name, _, dev = item.partition("=")
        if name.strip() not in plan:
            raise ValueError(f"unknown component in --plan: {name!r}")
        plan[name.strip()] = dev.strip().upper()
    return plan


def run(args, on_progress=lambda f: None):
    cache = os.path.abspath(args.cache_dir)
    model_dir = args.model_dir or os.path.join(cache, "models", MODEL_NAME)
    ov_cache = os.path.join(cache, "ov_cache")
    ensure_model(model_dir)
    prepare_ov_cache(ov_cache)

    t0 = time.perf_counter()
    engine = eng.Engine(model_dir, ov_cache, parse_plan(args.device, args.plan))
    load_secs = time.perf_counter() - t0

    channels = [("me", args.me)] + ([("others", args.others)] if args.others else [])
    audio = {name: read_wav(path) for name, path in channels}
    total_chunks = sum(len(p.plan_chunks(len(s))) for s in audio.values()) or 1
    done = 0

    result = {}
    t1 = time.perf_counter()
    for name, samples in audio.items():
        base = done

        def on_chunk(i, n, base=base):
            on_progress((base + i) / total_chunks)
        words = eng.transcribe_samples(engine, samples, on_chunk)
        done += len(p.plan_chunks(len(samples)))
        result[name] = p.segments_from_words(words)
    infer_secs = time.perf_counter() - t1
    audio_secs = sum(len(s) for s in audio.values()) / p.SAMPLE_RATE

    result.update({
        "model": MODEL_NAME,
        "device": "+".join(f"{k}:{v}" for k, v in engine.placement.items() if k != "mel"),
        "placement": engine.placement,
        "audio_secs": audio_secs,
        "load_secs": load_secs,
        "compile_secs": engine.compile_secs,
        "infer_secs": infer_secs,
        "stage_secs": engine.times,
        "calls": engine.calls,
        "rtfx": audio_secs / infer_secs if infer_secs > 0 else None,
    })
    return result


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--me", help="16 kHz mono WAV of the microphone channel")
    ap.add_argument("--others", help="16 kHz mono WAV of the system-audio channel")
    ap.add_argument("--cache-dir", required=True, help="cache root (models, ov_cache, tmp)")
    ap.add_argument("--model-dir", help="override the model folder")
    ap.add_argument("--device", default="auto", help="auto | npu | gpu | cpu")
    ap.add_argument("--plan", help='per-component override, e.g. "encoder=NPU;joint=CPU"')
    ap.add_argument("--download-only", action="store_true", help="fetch/verify the model and exit")
    args = ap.parse_args(argv)

    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    try:
        if args.download_only:
            ensure_model(args.model_dir or os.path.join(os.path.abspath(args.cache_dir), "models", MODEL_NAME))
            emit({"result": {"model": MODEL_NAME}})
            return 0
        if not args.me:
            ap.error("--me is required")
        last = [-1.0]

        def on_progress(f):
            if f - last[0] >= 0.01 or f >= 1.0:
                last[0] = f
                emit({"progress": round(f, 3)})
        emit({"result": run(args, on_progress)})
        return 0
    except Exception as e:
        eng.log(f"transcription failed: {type(e).__name__}: {e}")
        emit({"error": f"{type(e).__name__}: {e}"})
        return 1


if __name__ == "__main__":
    sys.exit(main())
