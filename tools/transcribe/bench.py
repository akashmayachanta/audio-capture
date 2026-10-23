"""Benchmark the transcription helper per device: cold vs warm compile and RTFx.

    <venv>\\python bench.py --me me.wav --others others.wav [--devices NPU GPU CPU]
                           [--cache-dir <root>] [--bench-cache <scratch>] [--show-text]

Each device is run twice as a separate process: first with an empty
compiled-model cache (cold), then again (warm). The model folder is shared with
the real cache root; compiled blobs go to --bench-cache so the real ov_cache is
untouched. Convert a recording first, e.g.
    ffmpeg -ss 120 -t 180 -i rec.opus -filter_complex "[0:a]channelsplit=channel_layout=stereo[l][r]" \\
        -map "[l]" -ar 16000 -ac 1 me.wav -map "[r]" -ar 16000 -ac 1 others.wav
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))


def default_cache():
    return os.environ.get("MEETREC_CACHE") or os.path.join(os.environ["LOCALAPPDATA"], "MeetRec")


def run_once(args, device, plan, bench_cache):
    cmd = [sys.executable, os.path.join(HERE, "transcribe.py"), "--me", args.me,
           "--cache-dir", bench_cache, "--model-dir", os.path.join(args.cache_dir, "models", "parakeet-tdt-0.6b-v3-ov"),
           "--device", device]
    if args.others:
        cmd += ["--others", args.others]
    if plan:
        cmd += ["--plan", plan]
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8")
    for line in reversed(proc.stdout.splitlines()):
        msg = json.loads(line)
        if "result" in msg:
            return msg["result"]
        if "error" in msg:
            return {"error": msg["error"], "stderr": proc.stderr[-1500:]}
    return {"error": f"no result (exit {proc.returncode})", "stderr": proc.stderr[-1500:]}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--me", required=True)
    ap.add_argument("--others")
    ap.add_argument("--cache-dir", default=default_cache())
    ap.add_argument("--bench-cache", help="scratch cache root for compiled blobs (default: a temp folder)")
    ap.add_argument("--devices", nargs="+", default=["NPU", "GPU", "CPU", "AUTO"])
    ap.add_argument("--plan", help='extra per-component override applied to every device run')
    ap.add_argument("--show-text", action="store_true")
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    bench_cache = args.bench_cache or tempfile.mkdtemp(prefix="meetrec-bench-")
    rows = []
    for device in args.devices:
        shutil.rmtree(os.path.join(bench_cache, "ov_cache"), ignore_errors=True)
        for label in ("cold", "warm"):
            print(f"{device} {label} ...", flush=True)
            r = run_once(args, device, args.plan, bench_cache)
            if "error" in r:
                print(f"  FAILED: {r['error']}\n{r['stderr']}")
                break
            rows.append((device, label, r))
            if args.show_text and label == "warm":
                for ch in ("me", "others"):
                    for s in r.get(ch, []):
                        print(f"  [{ch} {s['start']:7.1f}] {s['text']}")

    print()
    print(f"{'device':6} {'run':5} {'load s':>7} {'infer s':>8} {'RTFx':>6}  {'mel':>5} {'enc':>6} {'dec':>6} {'joint':>6}  placement")
    for device, label, r in rows:
        st = r["stage_secs"]
        print(f"{device:6} {label:5} {r['load_secs']:7.1f} {r['infer_secs']:8.1f} {r['rtfx']:6.1f}  "
              f"{st['mel']:5.1f} {st['encoder']:6.1f} {st['decoder']:6.1f} {st['joint']:6.1f}  {r['device']}")
    if rows:
        r = rows[-1][2]
        print(f"\naudio {r['audio_secs']:.0f}s, decoder calls {r['calls']['decoder']}, joint calls {r['calls']['joint']}")
    if not args.bench_cache:
        shutil.rmtree(bench_cache, ignore_errors=True)


if __name__ == "__main__":
    main()
