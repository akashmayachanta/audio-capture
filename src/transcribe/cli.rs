//! `meetrec.exe --transcribe <file.opus> [--device auto|npu|gpu|cpu] [--force] [--bench]
//! [--job-id ID] [--clear-cache ov|tmp|all]`
//!
//! The Transcripts window runs this as a child process, so a job survives the
//! window closing; its progress goes to `<cache>\tmp\<job-id>\progress.json`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::{anyhow, bail, Result};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;

use super::job::{self, Device, JobOptions};
use super::paths::{format_size, CacheDirs, ClearKind};
use super::progress::{self, JobState, Phase};
use super::transcript;
use crate::config::{self, Config};

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub file: Option<PathBuf>,
    pub device: Option<Device>,
    /// Replace an existing transcript.
    pub force: bool,
    /// Print timings and the transcript instead of writing the `.md`.
    pub bench: bool,
    pub job_id: Option<String>,
    pub clear_cache: Option<ClearKind>,
}

pub const USAGE: &str = "usage: meetrec --transcribe <file.opus> [--device auto|npu|gpu|cpu] [--force] [--bench] \
                         [--job-id ID]\n       meetrec --transcribe --clear-cache ov|tmp|all";

/// Parses what follows `--transcribe`.
pub fn parse_args(args: &[String]) -> Result<Args> {
    let mut out = Args::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| anyhow!("{name} needs a value"));
        match a.as_str() {
            "--device" => {
                let v = value("--device")?;
                if !Device::ALL.iter().any(|d| d.as_arg().eq_ignore_ascii_case(v)) {
                    bail!("unknown device {v:?} (use auto, npu, gpu or cpu)");
                }
                out.device = Some(Device::parse(v));
            }
            "--job-id" => out.job_id = Some(value("--job-id")?.clone()),
            "--clear-cache" => out.clear_cache = Some(value("--clear-cache")?.parse()?),
            "--force" => out.force = true,
            "--bench" => out.bench = true,
            flag if flag.starts_with("--") => bail!("unknown option {flag}"),
            file if out.file.is_none() => out.file = Some(PathBuf::from(file)),
            extra => bail!("unexpected argument {extra}"),
        }
    }
    Ok(out)
}

pub fn run(args: &[String]) -> Result<()> {
    let args = parse_args(args)?;
    let config = Config::load_or_create(&config::config_path());
    let cache = CacheDirs::from_config(&config.transcription.cache_dir);

    if let Some(kind) = args.clear_cache {
        let freed = cache.clear(kind)?;
        println!("Cleared {kind:?} under {}: freed {}", cache.root().display(), format_size(freed));
        if args.file.is_none() {
            return Ok(());
        }
    }
    let Some(opus) = args.file else { bail!("{USAGE}") };
    if !opus.is_file() {
        bail!("{} does not exist", opus.display());
    }
    if !args.bench && !args.force && transcript::md_path(&opus).exists() {
        bail!("{} already exists (use --force to replace it)", transcript::md_path(&opus).display());
    }

    // One job at a time: the NPU is not shared well.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\MeetRec.Transcribe"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        bail!("another transcription is already running");
    }

    let script = job::find_script().ok_or_else(|| anyhow!("tools\\transcribe\\transcribe.py not found next to the exe"))?;
    let device = args.device.unwrap_or_else(|| Device::parse(&config.transcription.device));
    let job_id = args.job_id.unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
    let opts = JobOptions {
        cache: cache.clone(),
        device,
        python_exe: config.transcription.python_exe.clone(),
        script,
        job_id: job_id.clone(),
        log: Some(config::app_dir().join("logs").join("transcribe.log")),
    };

    let state_file = progress::state_path(&cache, &job_id);
    let state = |phase, progress, message: &str| JobState {
        recording: opus.clone(),
        pid: std::process::id(),
        phase,
        progress,
        message: message.to_string(),
    };
    let report = |s: &JobState| {
        if let Err(e) = progress::write(&state_file, s) {
            tracing::warn!("progress file: {e:#}");
        }
    };
    report(&state(Phase::Running, 0.0, ""));
    tracing::info!("transcribing {} on {}", opus.display(), device.as_arg());

    let mut last = 0.0f32;
    let outcome = job::transcribe(&opus, &opts, &AtomicBool::new(false), |p| {
        if p - last >= 0.01 || p >= 1.0 {
            last = p;
            report(&state(Phase::Running, p, ""));
            eprint!("\r{:3.0}%", p * 100.0);
        }
    });
    eprintln!();

    match outcome {
        Ok(t) => {
            println!(
                "{} segments, {:.0}s of audio, {} on {}{}",
                t.segments.len(),
                t.duration_secs,
                t.model,
                t.device,
                t.rtfx.map(|r| format!(", {r:.1}x real time")).unwrap_or_default()
            );
            if args.bench {
                println!("{}", transcript::to_markdown(&t));
                let _ = std::fs::remove_dir_all(cache.tmp_job_dir(&job_id));
            } else {
                let path = transcript::write(&opus, &t, args.force)?;
                println!("Wrote {}", path.display());
                let _ = std::fs::remove_dir_all(cache.tmp_job_dir(&job_id)); // holds only the progress file now
            }
            Ok(())
        }
        Err(e) => {
            // The scratch folder is gone by now; leave just the failure for the window.
            report(&state(Phase::Failed, last, &format!("{e:#}")));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        parse_args(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_file_and_flags_parse() {
        let a = parse(&["D:\\rec\\a.opus", "--device", "NPU", "--force", "--job-id", "j1"]).unwrap();
        assert_eq!(a.file, Some(PathBuf::from("D:\\rec\\a.opus")));
        assert_eq!(a.device, Some(Device::Npu));
        assert!(a.force && !a.bench);
        assert_eq!(a.job_id.as_deref(), Some("j1"));
    }

    #[test]
    fn flags_may_come_before_the_file() {
        let a = parse(&["--bench", "--device", "cpu", "a.opus"]).unwrap();
        assert!(a.bench);
        assert_eq!(a.file, Some(PathBuf::from("a.opus")));
    }

    #[test]
    fn clear_cache_needs_no_file() {
        let a = parse(&["--clear-cache", "tmp"]).unwrap();
        assert_eq!(a.clear_cache, Some(ClearKind::Tmp));
        assert_eq!(a.file, None);
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(parse(&["--device", "tpu", "a.opus"]).is_err());
        assert!(parse(&["--device"]).is_err());
        assert!(parse(&["--clear-cache", "models"]).is_err());
        assert!(parse(&["--frobnicate"]).is_err());
        assert!(parse(&["a.opus", "b.opus"]).is_err());
    }

    #[test]
    fn no_arguments_is_an_empty_request() {
        assert_eq!(parse(&[]).unwrap(), Args::default());
    }
}
