# Faster Bark (OpenVINO encoder) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bark transcribes at 10x real time or better on Intel graphics and reloads without unpacking `model.bark`.

**Architecture:** `bark_loader` gains a cache of the unpacked weights and an OpenVINO export of the encoder. `BarkEngine` picks a backend from `settings.device` (`auto` | `cpu` | `gpu` | `cuda`); on the GPU backend `model.encoder.forward` calls the compiled OpenVINO model and the TDT decode stays in PyTorch.

**Tech Stack:** Python 3.11, torch 2.8.0 (CPU), transformers 5.19.0, openvino 2026.4.1, safetensors (already installed with transformers).

**Spec:** `docs/superpowers/specs/2026-11-17-bark-openvino-design.md`

## Global Constraints

- No git commits, branches or pushes (project rule). Steps have no commit step.
- No test cases for `backend/transcriber` (project rule). Each task is verified by a live run.
- Live runs use `AUDCAP_TRANSCRIBER_SETTINGS` pointing at a scratch `settings.toml` with a scratch `cache_dir` and `recordings_dir`; the user's cache in `%LOCALAPPDATA%\AudCap\transcriber` is only read (copy `models\underdog-bark-3.0\model.bark` and `hf\` from it).
- Never execute downloaded code; downloads stay pinned to revisions.
- Cache files are written to a temp name and renamed.
- Run Python from PowerShell in `backend/transcriber` with `.venv\Scripts\python.exe`.

## Review Focus

- First load interrupted mid-conversion: next load must redo it, not read a partial file (temp + rename; Task 1 and 2 verification deletes/creates a `.tmp` leftover).
- `device = "auto"` on a machine with no OpenVINO `GPU` device, or where compiling fails: Bark loads on CPU and the log says why (Task 2 verification forces a failure with env `AUDCAP_BARK_FAIL_GPU=1`... not shipped: instead verify by calling the fallback with a bogus device name in the check script).
- `device = "gpu"` without a GPU: load fails with a readable error in `/api/state` `engine.error`, no fallback.
- Last batch of a job smaller than `BATCH`, and a single 0.3 s piece: texts line up with pieces (Task 2 check runs 1, 2 and 5 pieces).
- Old `settings.toml` with `device = "cpu"` keeps working unchanged.

---

### Task 1: Weight cache and offline configs (`bark_loader.py`)

**Files:**
- Modify: `backend/transcriber/app/bark_loader.py`

**Interfaces:**
- Produces:
  - `cached_state_dict(path, rev) -> (dict[str, np.ndarray], dict)`: reads `unpacked-<rev[:8]>.safetensors` + `config-<rev[:8]>.json` next to `path`; when missing, calls `load_state_dict(path)`, saves floats as fp16 (ints unchanged) via `safetensors.numpy.save_file` to `*.tmp` then `os.replace`, config last.
  - `build_model(sd, cfg, cache_dir, skip_encoder=False) -> (ParakeetForTDT, processor)`: today's `load_model` body minus the unpack; `GenerationConfig`/`AutoProcessor` are tried with `local_files_only=True` first and again without it on `OSError`. With `skip_encoder=True`, keys starting with `encoder.` are not loaded and the missing-keys check ignores them.
  - `load_model(path, cache_dir, device="cpu", rev="")` keeps its signature plus `rev`; uses the two functions above.

- [ ] **Step 1:** Implement the three functions.
- [ ] **Step 2:** Verify with `scratchpad/check1.py`: load twice from the scratch cache; print load seconds for each and transcribe the saved `out/pieces.npz` pieces. Expected: second load has no "unpack" phase (well under 30 s total after imports), texts equal `out/baseline.json`.
- [ ] **Step 3:** Put a zero-byte `unpacked-<rev8>.safetensors.tmp` in the folder, delete the real one, load again. Expected: rebuilt, loads fine.

### Task 2: OpenVINO GPU backend and `device = "auto"`

**Files:**
- Modify: `backend/transcriber/app/bark_loader.py`, `backend/transcriber/app/engines.py`, `backend/transcriber/app/settings.py`, `backend/transcriber/settings.toml`, `backend/transcriber/pyproject.toml` (+ `uv lock`)

**Interfaces:**
- Consumes: `cached_state_dict`, `build_model` from Task 1.
- Produces:
  - `bark_loader.openvino_gpu() -> bool`: `"GPU" in openvino.Core().available_devices`, `False` when openvino cannot be imported.
  - `bark_loader.export_encoder(model, processor, xml_path)`: wraps `model.encoder` in a module `forward(input_features, attention_mask) -> (last_hidden_state, attention_mask)`, `openvino.convert_model(..., example_input=...)` with a 2-row example, `openvino.save_model(..., compress_to_fp16=True)` to temp names then rename (`.bin` first, `.xml` last).
  - `bark_loader.load_openvino(path, cache_root, rev, ov_device="GPU") -> (model, processor)`: IR at `<model dir>/encoder-<rev[:8]>.xml`; when missing, full `build_model` + `export_encoder`; otherwise `build_model(skip_encoder=True)`. Compiles with `CACHE_DIR = <cache_root>/ov_cache`, `PERFORMANCE_HINT = LATENCY`, and sets `model.encoder.forward` to a function returning `ParakeetEncoderModelOutput(last_hidden_state=…, attention_mask=….int())`.
  - `BarkEngine(cache_root, device="auto")`: `auto` → OpenVINO when `openvino_gpu()`, falling back to CPU with `log.warning` on any exception; `gpu` → OpenVINO, errors propagate; `cpu`/`cuda` as today. `self.backend in ("openvino", "torch")`. `device_label`: `"Intel GPU (OpenVINO)"` or today's.
  - `BarkEngine.FRAME_BUCKET = 256`: on the OpenVINO backend each batch is padded to `BATCH` rows by repeating the last wave, and `input_features`/`attention_mask` are right-padded with zeros to a multiple of `FRAME_BUCKET` frames; only the real rows' texts are kept.
  - `Settings.device` default `"auto"`; `settings.toml` comment lists the four values.
  - `pyproject.toml`: `"openvino==2026.4.1"`.

- [ ] **Step 1:** Add the dependency, run `uv lock` and `uv sync`.
- [ ] **Step 2:** Implement loader and engine changes.
- [ ] **Step 3:** Verify with `scratchpad/check2.py` against the scratch cache: `BarkEngine(cache, "auto")` twice in separate processes; transcribe the `pieces.npz` pieces through `engine.transcribe` with real `Piece` objects (also 1, 2 and 5 pieces). Expected: label `Intel GPU (OpenVINO)`; second process loads without unpack/convert; warm speed ≥ 10x; at most a few words differ from `out/baseline.json`; texts align with pieces.
- [ ] **Step 4:** Verify `BarkEngine(cache, "cpu")` still matches the baseline exactly, and that a failing GPU load under `auto` (check script monkeypatches `bark_loader.load_openvino` to raise) ends on CPU with a warning, while `"gpu"` raises.
- [ ] **Step 5:** Start the server with the scratch settings (`model = "bark"`, another port), `GET /api/state` until `engine.loaded == "bark"`, check `engine.device`; transcribe one copied recording via `POST /api/transcribe`; read the `.md` header's device and speed; `POST /api/shutdown`.

### Task 3: Docs

**Files:**
- Modify: `backend/transcriber/app/engines.py` (`CATALOG` Bark description, class docstring), `README.md`, `Claude.md`

- [ ] **Step 1:** Update the Bark description, `device` values, measured numbers (from Task 2's runs), cache contents and size, and the `engines.py`/`bark_loader.py` rows in `Claude.md`.
