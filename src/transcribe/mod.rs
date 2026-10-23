//! On-device transcription: Parakeet TDT on OpenVINO (NPU first), run by a
//! Python helper (`tools/transcribe`). Everything here is the Rust side:
//! the cache folder, audio preparation, the transcript file and the job runner.

pub mod audio_prep;
pub mod cli;
pub mod job;
pub mod paths;
pub mod progress;
pub mod transcript;
