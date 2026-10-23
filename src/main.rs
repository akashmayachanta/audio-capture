#![windows_subsystem = "windows"]

use std::fs::File;
use std::io::BufWriter;
use std::time::Duration;

use anyhow::{Context, Result};
use meetrec::audio::capture::{CaptureStream, Source};
use meetrec::audio::mixer::Mixer;
use meetrec::audio::SAMPLE_RATE;
use meetrec::config;
use meetrec::settings_ui::Tab;
use meetrec::encode::opus_ogg::OggOpusWriter;
use ringbuf::traits::Split;
use ringbuf::HeapRb;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
use windows::Win32::System::Threading::CreateMutexW;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--record-test") => {
            // GUI-subsystem exe: borrow the parent's console for CLI output.
            unsafe {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
            tracing_subscriber::fmt().with_env_filter("info").init();
            let secs: u64 = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(60);
            let path = args.get(3).cloned().unwrap_or_else(|| "record-test.opus".into());
            record_test(secs, &path)
        }
        Some("--transcribe") => run_transcribe(&args[2..]),
        Some("--transcripts") => run_transcripts(),
        Some("--settings") => run_settings(Tab::Settings),
        Some("--recordings") => run_settings(Tab::Recordings),
        _ => run_tray(),
    }
}

/// Prints to the parent's console when there is one; spawned by the Transcripts
/// window it has none, so it logs to the file instead.
fn run_transcribe(args: &[String]) -> Result<()> {
    let _log_guard = if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
        tracing_subscriber::fmt().with_env_filter("info").init();
        None
    } else {
        Some(init_file_logging()?)
    };
    let result = meetrec::transcribe::cli::run(args);
    if let Err(e) = &result {
        tracing::error!("transcribe: {e:#}");
    }
    result
}

fn run_transcripts() -> Result<()> {
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\MeetRec.Transcripts"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(()); // a transcripts window is already open
    }
    meetrec::transcripts_ui::run(config::config_path())
}

fn run_settings(tab: Tab) -> Result<()> {
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\MeetRec.Settings"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(()); // a settings window is already open
    }
    meetrec::settings_ui::run(config::config_path(), tab)
}

fn run_tray() -> Result<()> {
    let _log_guard = init_file_logging()?;
    // One tray process per user session.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\MeetRec.Tray"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        tracing::info!("already running");
        return Ok(());
    }
    std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
    let result = meetrec::app::App::new().and_then(|app| app.run());
    if let Err(e) = &result {
        tracing::error!("fatal: {e:#}");
    }
    result
}

fn init_file_logging() -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let dir = config::app_dir().join("logs");
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("meetrec")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = tracing_subscriber::EnvFilter::try_from_env("MEETREC_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_env_filter(filter).init();
    Ok(guard)
}

/// Spike: record `secs` of mic (L) + system (R) straight to an Ogg Opus file.
fn record_test(secs: u64, path: &str) -> Result<()> {
    let (mp, mc) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 2).split();
    let (sp, sc) = HeapRb::<f32>::new(SAMPLE_RATE as usize * 2).split();
    let mic = CaptureStream::spawn(Source::Mic, None, mp);
    let sys = CaptureStream::spawn(Source::System, None, sp);
    let mut mixer = Mixer::new(Some(mc), Some(sc));
    let file = File::create(path).with_context(|| format!("create {path}"))?;
    let tags = [("MEETREC_CHANNELS", "left=mic,right=system")];
    let mut w = OggOpusWriter::new(BufWriter::new(file), mixer.channels() as u8, 32_000, &tags)?;
    let mut frame = Vec::new();
    let total = secs * SAMPLE_RATE as u64;
    while w.samples_written() < total {
        while w.samples_written() < total && mixer.next_frame(&mut frame) {
            w.write_frame(&frame)?;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    w.finish()?;
    println!("wrote {path}: mic={:?} system={:?}", mic.device_name(), sys.device_name());
    Ok(())
}
