//! Runs one transcription: decode the recording to per-channel WAVs, hand them
//! to the Python helper (`tools/transcribe/transcribe.py`), read its JSON-lines
//! output and turn the result into a [`Transcript`].

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use super::audio_prep::{self, Prepared};
use super::paths::CacheDirs;
use super::transcript::{merge, Segment, Speaker, Transcript};
use crate::session::Meta;

/// Share of the progress bar taken by decoding the recording; the rest is the helper.
const PREP_SHARE: f32 = 0.05;

/// Job folders older than this are leftovers from a crash.
pub const STALE_TMP: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Device {
    /// Encoder on the NPU (GPU, then CPU, if it cannot compile there).
    #[default]
    Auto,
    Npu,
    Gpu,
    Cpu,
}

impl Device {
    pub const ALL: [Device; 4] = [Device::Auto, Device::Npu, Device::Gpu, Device::Cpu];

    /// The value of the helper's `--device` flag, also the config value.
    pub fn as_arg(self) -> &'static str {
        match self {
            Device::Auto => "auto",
            Device::Npu => "npu",
            Device::Gpu => "gpu",
            Device::Cpu => "cpu",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Device::Auto => "Auto (NPU first)",
            Device::Npu => "NPU",
            Device::Gpu => "GPU",
            Device::Cpu => "CPU",
        }
    }

    /// Unknown names fall back to `Auto`, so a hand-edited config never breaks transcription.
    pub fn parse(s: &str) -> Self {
        Self::ALL.into_iter().find(|d| d.as_arg().eq_ignore_ascii_case(s.trim())).unwrap_or_default()
    }
}

/// One line of the helper's stdout.
#[derive(Debug, PartialEq)]
pub enum Event {
    Progress(f32),
    Result(Box<RawResult>),
    Error(String),
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct RawSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The helper's `{"result": ...}` payload (extra fields such as timings are ignored).
#[derive(Debug, Deserialize, PartialEq)]
pub struct RawResult {
    #[serde(default)]
    pub me: Vec<RawSegment>,
    #[serde(default)]
    pub others: Vec<RawSegment>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub rtfx: Option<f64>,
}

/// `None` for lines that are not helper messages (stray prints, blank lines).
pub fn parse_line(line: &str) -> Option<Event> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if let Some(p) = v.get("progress").and_then(|p| p.as_f64()) {
        return Some(Event::Progress(p.clamp(0.0, 1.0) as f32));
    }
    if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
        return Some(Event::Error(e.to_string()));
    }
    let result = serde_json::from_value::<RawResult>(v.get("result")?.clone()).ok()?;
    Some(Event::Result(Box::new(result)))
}

fn to_segments(raw: Vec<RawSegment>, speaker: Speaker) -> Vec<Segment> {
    raw.into_iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|s| Segment { speaker, start: s.start, end: s.end, text: s.text })
        .collect()
}

/// Builds the transcript for `opus`, taking date/apps/duration from the sidecar when there is one.
pub fn to_transcript(raw: RawResult, opus: &Path, meta: Option<&Meta>, fallback_duration: f64) -> Transcript {
    Transcript {
        audio_file: opus.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        started: meta.map(|m| m.start),
        apps: meta.map(|m| m.apps.clone()).unwrap_or_default(),
        duration_secs: meta.map_or(fallback_duration, |m| m.duration_secs),
        model: raw.model,
        device: raw.device,
        rtfx: raw.rtfx,
        segments: merge(to_segments(raw.me, Speaker::Me), to_segments(raw.others, Speaker::Others)),
    }
}

/// Turns the helper's error text into something a person can act on.
pub fn friendly_error(msg: &str) -> String {
    if msg.contains("ModuleNotFoundError") || msg.contains("No module named") {
        format!("The Python environment is incomplete ({msg}). Run tools\\transcribe\\setup.ps1")
    } else if msg.contains("snapshot_download") || msg.contains("ConnectionError") || msg.contains("HTTPError") {
        format!("The model could not be downloaded; check the network connection ({msg})")
    } else {
        msg.to_string()
    }
}

/// Where `transcribe.py` lives: `MEETREC_TRANSCRIBE_SCRIPT`, else `tools\transcribe`
/// beside the exe or in a parent folder (the `target\release` layout).
pub fn find_script() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MEETREC_TRANSCRIBE_SCRIPT").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .map(|d| d.join("tools").join("transcribe").join("transcribe.py"))
        .find(|p| p.is_file())
}

pub struct JobOptions {
    pub cache: CacheDirs,
    pub device: Device,
    /// `transcription.python_exe`; empty = the cache's venv.
    pub python_exe: PathBuf,
    pub script: PathBuf,
    pub job_id: String,
    /// Where the helper's stderr goes.
    pub log: Option<PathBuf>,
}

impl JobOptions {
    pub fn python(&self) -> PathBuf {
        if self.python_exe.as_os_str().is_empty() { self.cache.venv_python() } else { self.python_exe.clone() }
    }

    pub fn args(&self, prepared: &Prepared) -> Vec<std::ffi::OsString> {
        let mut args: Vec<std::ffi::OsString> = vec![
            self.script.clone().into(),
            "--me".into(),
            prepared.me.clone().into(),
            "--cache-dir".into(),
            self.cache.root().into(),
            "--device".into(),
            self.device.as_arg().into(),
        ];
        if let Some(others) = &prepared.others {
            args.push("--others".into());
            args.push(others.clone().into());
        }
        args
    }

    fn command(&self, prepared: &Prepared) -> Result<Command> {
        let python = self.python();
        if !python.is_file() {
            bail!("Python not found at {}. Run tools\\transcribe\\setup.ps1", python.display());
        }
        if !self.script.is_file() {
            bail!("Transcription helper not found at {}", self.script.display());
        }
        let mut cmd = Command::new(python);
        cmd.args(self.args(prepared)).env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
        Ok(cmd)
    }
}

/// Runs `cmd` (the helper) to completion, reporting progress. Killing on
/// `cancel` takes the whole process down; returns `Err` then.
pub fn run_command(
    mut cmd: Command,
    log: Option<&Path>,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<RawResult> {
    hide_window(&mut cmd);
    let stderr = match log.and_then(open_log) {
        Some(file) => Stdio::from(file),
        None => Stdio::null(),
    };
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .context("could not start the transcription helper")?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(event) = parse_line(&line) {
                if tx.send(event).is_err() {
                    break;
                }
            }
        }
    });

    let mut result = None;
    let mut error = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("cancelled");
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Progress(p)) => on_progress(p),
            Ok(Event::Result(r)) => result = Some(r),
            Ok(Event::Error(e)) => error = Some(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    match (result, error) {
        (Some(r), _) if status.success() => Ok(*r),
        (_, Some(e)) => Err(anyhow!(friendly_error(&e))),
        _ => {
            let log = log.map(|l| format!(" (see {})", l.display())).unwrap_or_default();
            Err(anyhow!("the transcription helper exited with {status} without a result{log}"))
        }
    }
}

fn open_log(path: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

/// The exe has no console; without this a console window flashes for the helper.
fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Transcribes `opus`; progress is 0..=1 across decoding and the helper. The
/// job's scratch folder is removed when this returns, whatever the outcome.
pub fn transcribe(
    opus: &Path,
    opts: &JobOptions,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f32),
) -> Result<Transcript> {
    opts.cache.sweep_stale_tmp(STALE_TMP);
    let dir = opts.cache.tmp_job_dir(&opts.job_id);
    let _cleanup = RemoveOnDrop(dir.clone());
    let prepared = audio_prep::prepare(opus, &dir, |p| on_progress(p * PREP_SHARE))?;
    let cmd = opts.command(&prepared)?;
    let raw = run_command(cmd, opts.log.as_deref(), cancel, |p| on_progress(PREP_SHARE + p * (1.0 - PREP_SHARE)))?;
    let meta = std::fs::read(opus.with_extension("json")).ok().and_then(|b| serde_json::from_slice::<Meta>(&b).ok());
    Ok(to_transcript(raw, opus, meta.as_ref(), prepared.duration_secs))
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_json(extra: &str) -> String {
        format!(
            r#"{{"result":{{"me":[{{"start":1.0,"end":2.0,"text":"hello"}}],"others":[{{"start":0.5,"end":0.9,"text":"hi"}}],"model":"m","device":"encoder:NPU","rtfx":9.5{extra}}}}}"#
        )
    }

    #[test]
    fn devices_round_trip_and_unknown_means_auto() {
        for d in Device::ALL {
            assert_eq!(Device::parse(d.as_arg()), d);
        }
        assert_eq!(Device::parse(" NPU "), Device::Npu);
        assert_eq!(Device::parse("tpu"), Device::Auto);
        assert_eq!(Device::parse(""), Device::Auto);
    }

    #[test]
    fn progress_lines_are_clamped() {
        assert_eq!(parse_line(r#"{"progress": 0.42}"#), Some(Event::Progress(0.42)));
        assert_eq!(parse_line(r#"{"progress": 7}"#), Some(Event::Progress(1.0)));
    }

    #[test]
    fn error_lines_carry_the_message() {
        assert_eq!(parse_line(r#"{"error": "boom"}"#), Some(Event::Error("boom".into())));
    }

    #[test]
    fn result_lines_ignore_unknown_fields() {
        let Some(Event::Result(r)) = parse_line(&result_json(r#","stage_secs":{"encoder":1.0}"#)) else {
            panic!("not a result");
        };
        assert_eq!(r.me[0].text, "hello");
        assert_eq!(r.others.len(), 1);
        assert_eq!(r.rtfx, Some(9.5));
    }

    #[test]
    fn stray_output_is_ignored() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("Fetching 11 files: 100%"), None);
        assert_eq!(parse_line(r#"{"unrelated": 1}"#), None);
        assert_eq!(parse_line(r#"{"result": "not an object"}"#), None);
    }

    #[test]
    fn transcript_interleaves_channels_and_drops_empty_segments() {
        let Some(Event::Result(r)) = parse_line(&result_json("")) else { panic!() };
        let mut raw = *r;
        raw.me.push(RawSegment { start: 3.0, end: 3.1, text: "  ".into() });
        let t = to_transcript(raw, Path::new("D:\\rec\\a.opus"), None, 12.0);
        assert_eq!(t.audio_file, "a.opus");
        assert_eq!(t.duration_secs, 12.0);
        let order: Vec<_> = t.segments.iter().map(|s| (s.speaker, s.text.as_str())).collect();
        assert_eq!(order, [(Speaker::Others, "hi"), (Speaker::Me, "hello")]);
    }

    #[test]
    fn errors_are_translated_for_a_missing_environment() {
        assert!(friendly_error("ModuleNotFoundError: No module named 'openvino'").contains("setup.ps1"));
        assert_eq!(friendly_error("something else"), "something else");
    }

    #[test]
    fn args_name_both_channels_the_device_and_the_cache() {
        let cache = CacheDirs::new("C:\\cache");
        let opts = JobOptions {
            cache,
            device: Device::Npu,
            python_exe: PathBuf::new(),
            script: "S\\transcribe.py".into(),
            job_id: "j".into(),
            log: None,
        };
        let prepared =
            Prepared { me: "J\\me.wav".into(), others: Some("J\\others.wav".into()), duration_secs: 1.0 };
        let args: Vec<String> = opts.args(&prepared).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            ["S\\transcribe.py", "--me", "J\\me.wav", "--cache-dir", "C:\\cache", "--device", "npu", "--others", "J\\others.wav"]
        );
        assert_eq!(opts.python(), Path::new("C:\\cache\\venv\\Scripts\\python.exe"));
        let mono = Prepared { others: None, ..prepared };
        assert!(!opts.args(&mono).iter().any(|a| a == "--others"));
    }

    #[test]
    fn a_configured_python_wins_over_the_venv() {
        let opts = JobOptions {
            cache: CacheDirs::new("C:\\cache"),
            device: Device::Auto,
            python_exe: "D:\\py\\python.exe".into(),
            script: PathBuf::new(),
            job_id: String::new(),
            log: None,
        };
        assert_eq!(opts.python(), Path::new("D:\\py\\python.exe"));
    }

    /// A stand-in helper: a batch file that prints `lines` and exits with `code`.
    fn helper(dir: &Path, lines: &[String], code: i32) -> Command {
        let path = dir.join("helper.cmd");
        let mut body = String::from("@echo off\r\n");
        for l in lines {
            body.push_str(&format!("echo {l}\r\n"));
        }
        body.push_str(&format!("exit /b {code}\r\n"));
        std::fs::write(&path, body).unwrap();
        let mut c = Command::new("cmd");
        c.arg("/C").arg(path);
        c
    }

    #[test]
    fn runs_a_helper_and_collects_progress_and_result() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            r#"{"progress": 0.25}"#.to_string(),
            "noise".to_string(),
            r#"{"progress": 0.75}"#.to_string(),
            result_json(""),
        ];
        let mut seen = Vec::new();
        let r = run_command(helper(dir.path(), &lines, 0), None, &AtomicBool::new(false), |p| seen.push(p)).unwrap();
        assert_eq!(seen, vec![0.25, 0.75]);
        assert_eq!(r.me[0].text, "hello");
        assert_eq!(r.device, "encoder:NPU");
    }

    #[test]
    fn a_helper_error_is_reported_with_its_message() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [r#"{"error": "ValueError: bad wav"}"#.to_string()];
        let err = run_command(helper(dir.path(), &lines, 1), None, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(err.to_string().contains("bad wav"), "{err}");
    }

    #[test]
    fn a_result_with_a_failing_exit_code_is_still_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_command(helper(dir.path(), &[result_json("")], 2), None, &AtomicBool::new(false), |_| {})
            .unwrap_err();
        assert!(err.to_string().contains("without a result"), "{err}");
    }

    #[test]
    fn exiting_without_a_result_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_command(helper(dir.path(), &[], 3), None, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(err.to_string().contains("without a result"), "{err}");
    }

    #[test]
    fn stderr_goes_to_the_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let lines = ["to the log 1>&2".to_string(), result_json("")];
        let log = dir.path().join("logs").join("transcribe.log");
        run_command(helper(dir.path(), &lines, 0), Some(&log), &AtomicBool::new(false), |_| {}).unwrap();
        assert!(std::fs::read_to_string(&log).unwrap().contains("to the log"));
    }

    /// Removes a directory junction (never what it points at) when dropped, so the
    /// temp folder's cleanup cannot follow it into the real model.
    struct Junction(PathBuf);

    impl Drop for Junction {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    /// Needs `tools\transcribe\setup.ps1 -DownloadModel` (venv + model in the default
    /// cache) and an Intel NPU: `cargo test --lib -- --ignored hardware`.
    #[test]
    #[ignore]
    fn hardware_transcribes_a_short_clip_on_the_npu() {
        use crate::audio::FRAME;
        use crate::encode::opus_ogg::OggOpusWriter;

        let real = CacheDirs::new(super::super::paths::default_root());
        assert!(real.venv_python().is_file() && real.model_dir().is_dir(), "run setup.ps1 -DownloadModel first");
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheDirs::new(dir.path().join("cache"));
        std::fs::create_dir_all(cache.root()).unwrap();
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(cache.models_dir())
            .arg(real.models_dir())
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
        let _junction = Junction(cache.models_dir());

        // Five seconds of a 300 Hz tone on both channels: enough to exercise every stage.
        let opus = dir.path().join("tone.opus");
        let mut w = OggOpusWriter::new(Vec::new(), 2, 48_000, &[]).unwrap();
        for f in 0..5 * 50 {
            let mut pcm = Vec::new();
            for n in f * FRAME..(f + 1) * FRAME {
                let s = (n as f32 * 300.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.3;
                pcm.extend([s, s]);
            }
            w.write_frame(&pcm).unwrap();
        }
        std::fs::write(&opus, w.finish().unwrap()).unwrap();

        let opts = JobOptions {
            cache: cache.clone(),
            device: Device::Npu,
            python_exe: real.venv_python(),
            script: find_script().expect("tools/transcribe/transcribe.py"),
            job_id: "hw".into(),
            log: None,
        };
        let mut last = 0.0;
        let t = transcribe(&opus, &opts, &AtomicBool::new(false), |p| last = p).unwrap();
        assert_eq!(last, 1.0);
        assert!(t.device.contains("encoder:NPU"), "ran on {}", t.device);
        assert!(t.rtfx.is_some());
        assert!(cache.ov_cache_dir().is_dir(), "compiled blobs are cached under the job's cache folder");
        assert!(!cache.tmp_job_dir("hw").exists(), "scratch folder is removed");
    }

    #[test]
    fn cancelling_stops_the_helper() {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", "ping -n 30 127.0.0.1 > nul"]);
        let started = std::time::Instant::now();
        let err = run_command(cmd, None, &AtomicBool::new(true), |_| {}).unwrap_err();
        assert_eq!(err.to_string(), "cancelled");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
