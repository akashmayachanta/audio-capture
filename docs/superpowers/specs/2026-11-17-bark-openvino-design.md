# Faster Bark: encoder on Intel graphics (OpenVINO)

Status: approved by the user on 2026-11-17. Not committed (project rule: the user commits).

## Intent and why

Bark (Underdog Bark 3.0, a Parakeet TDT 0.6B v3 fine-tune) is the accurate
model but runs at about 4x real time on the user's laptop (Core Ultra 7 155U,
Intel integrated graphics, 16 GB) and takes over a minute to load at every
server start. The user wants it faster, inside `backend/transcriber`.

## Measured (154 s of speech from one Teams recording)

| Setup | Speed | Words changed vs today (of 401) |
|---|---|---|
| PyTorch CPU (today) | 3.5-4.0x | - |
| OpenVINO CPU | 3.8x | 0 |
| OpenVINO Intel graphics (fp16) | 11-14x warm | 2-3 (punctuation / sentence breaks) |
| PyTorch dynamic int8 linear | 3.0x | 72 |

The encoder is 85% of transcription time. Load today: unpack `model.bark` 46 s,
build the empty model 14-17 s, config/processor 8 s (3 s with
`local_files_only`). Unpacked weights saved as fp16 safetensors load in 5 s
and gave identical text. On the graphics chip: convert 45 s and compile 55 s
once; every new input shape costs about 8 s once; all of it is kept in
OpenVINO's cache dir (a later process compiled in 10-16 s and ran its first
pass at 8.6x).

## Goals

- Bark at 10x real time or better on Intel graphics, with no setup by the user.
- Later Bark loads skip the `.bark` unpack.
- Machines without a usable Intel GPU behave as today.

## Non-goals

- NPU, DirectML/ONNX, other models, the Windows speech API (dropped by the user).
- Moving the TDT decode loop off PyTorch.
- Tests for the Python app (project rule).

## Anti-goals

- A failed GPU setup leaving Bark unusable: `auto` must fall back to CPU and say so in the log.
- Running downloaded code, or unpinned downloads.

## Approaches

1. **OpenVINO for the encoder on the Intel GPU, PyTorch for the rest. Chosen** (3x measured gain, small change: one module swapped).
2. PyTorch tuning (threads, batch, int8). Rejected: no gain, int8 changes 72/401 words.
3. ONNX Runtime + DirectML. Not tried; needs a separate export path and OpenVINO already delivers on this hardware.

## Design

- `settings.toml` `device`: `"auto"` (new default), `"cpu"`, `"gpu"`, `"cuda"`.
  `auto` = OpenVINO GPU when OpenVINO lists a `GPU` device, else PyTorch CPU;
  if the GPU path fails to load, log it and use CPU. `"gpu"` forces OpenVINO
  GPU and reports the error instead of falling back.
- Cache layout under `<cache>/models/underdog-bark-3.0/`, keyed by the pinned
  `REVISION` so a new model revision rebuilds:
  - `unpacked-<rev8>.safetensors` + `config-<rev8>.json`: all weights as fp16
    (CPU path, and the source for conversion).
  - `encoder-<rev8>.xml/.bin`: the encoder as OpenVINO IR (fp16), converted
    once from the PyTorch encoder; `<cache>/ov_cache` holds compiled blobs.
  - Files are written to a temp name and renamed, so an interrupted first load
    is redone rather than half-read.
- GPU load: read safetensors, build the PyTorch model (decoder, joint and
  projector are used; the PyTorch encoder is dropped after conversion or never
  filled when the IR exists), compile the IR, replace `model.encoder.forward`
  with a call into OpenVINO returning `ParakeetEncoderModelOutput`.
- Fixed shapes on GPU: every batch is padded to `BATCH` rows (repeat the last
  piece) and the frame count to a multiple of 256, with the attention mask
  marking the padding, so only a handful of shapes are ever compiled.
- Config and processor are read with `local_files_only=True` first, falling
  back to the network when not yet cached.
- `device_label`: `Intel GPU (OpenVINO)` or `CPU (n threads)`.
- `pyproject.toml`: `openvino` pinned to the tested version; `uv.lock` updated.
- Docs: `settings.toml` comments, CATALOG description, README/Claude.md numbers.

## Left to the builder

- Whether to skip building/filling the PyTorch encoder when the IR exists
  (saves memory and load time) or keep it simple.
- Exact fallback/error wording; whether to warm the common shapes at load.

## Calls that are mine, not the user's

`auto` as the default, fp16 weight cache, 256-frame buckets, keeping about
2.4 GB of converted model and compile cache on disk.
