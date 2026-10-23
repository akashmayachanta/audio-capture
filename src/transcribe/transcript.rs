//! The transcript of one recording and its Markdown file, which sits next to
//! the voice file with the same base name (`…_Teams.opus` -> `…_Teams.md`).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::recordings::format_time;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    /// Left channel: the microphone.
    Me,
    /// Right channel: system audio, i.e. the other participants.
    Others,
}

impl Speaker {
    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "Me",
            Speaker::Others => "Others",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub speaker: Speaker,
    /// Seconds from the start of the recording.
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    /// File name of the recording, e.g. `2026-10-19_1216_Teams.opus`.
    pub audio_file: String,
    /// From the sidecar, when there is one.
    pub started: Option<DateTime<Local>>,
    pub apps: Vec<String>,
    pub duration_secs: f64,
    pub model: String,
    pub device: String,
    /// Audio seconds transcribed per second of work.
    pub rtfx: Option<f64>,
    pub segments: Vec<Segment>,
}

/// Interleaves the two channels by start time. Equal starts keep "me" first.
pub fn merge(mut me: Vec<Segment>, others: Vec<Segment>) -> Vec<Segment> {
    me.extend(others);
    me.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| (a.speaker as u8).cmp(&(b.speaker as u8))));
    me
}

/// `00:01:23`.
pub fn timestamp(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

pub fn to_markdown(t: &Transcript) -> String {
    let mut out = String::new();
    let title = match t.started {
        Some(s) => {
            let who = if t.apps.is_empty() { String::new() } else { format!(" · {}", t.apps.join(", ")) };
            format!("{}{who}", s.format("%Y-%m-%d %H:%M"))
        }
        None => Path::new(&t.audio_file).file_stem().unwrap_or_default().to_string_lossy().into_owned(),
    };
    out.push_str(&format!("# Transcript: {title}\n\n"));
    out.push_str(&format!("- **Recording:** `{}`\n", t.audio_file));
    if let Some(s) = t.started {
        out.push_str(&format!("- **Date:** {}\n", s.format("%Y-%m-%d %H:%M")));
    }
    if !t.apps.is_empty() {
        out.push_str(&format!("- **Apps:** {}\n", t.apps.join(", ")));
    }
    out.push_str(&format!("- **Duration:** {}\n", format_time(t.duration_secs)));
    let speed = t.rtfx.map(|r| format!(", {r:.1}x real time")).unwrap_or_default();
    out.push_str(&format!("- **Transcribed with:** {} on {}{speed}\n", t.model, t.device));
    out.push_str(
        "- **Model:** NVIDIA Parakeet TDT 0.6B v3 (CC-BY-4.0), OpenVINO conversion by FluidInference\n\n---\n\n",
    );
    if t.segments.is_empty() {
        out.push_str("_No speech detected._\n");
    }
    for s in &t.segments {
        out.push_str(&format!("**[{}] {}:** {}\n\n", timestamp(s.start), s.speaker.label(), s.text.trim()));
    }
    out
}

/// `<same base name>.md` next to the voice file.
pub fn md_path(opus: &Path) -> PathBuf {
    opus.with_extension("md")
}

/// Writes the transcript next to `opus`. An existing `.md` is only replaced
/// when `overwrite` is set (an explicit re-transcribe). Written to a temp file
/// and renamed, so a crash never leaves a torn transcript.
pub fn write(opus: &Path, t: &Transcript, overwrite: bool) -> Result<PathBuf> {
    let path = md_path(opus);
    if path.exists() && !overwrite {
        bail!("{} already exists", path.display());
    }
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, to_markdown(t)).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn seg(speaker: Speaker, start: f64, text: &str) -> Segment {
        Segment { speaker, start, end: start + 1.0, text: text.into() }
    }

    fn transcript(segments: Vec<Segment>) -> Transcript {
        Transcript {
            audio_file: "2026-10-19_1216_Teams.opus".into(),
            started: Some(Local.with_ymd_and_hms(2026, 10, 19, 12, 16, 0).unwrap()),
            apps: vec!["Teams".into()],
            duration_secs: 1043.6,
            model: "parakeet-tdt-0.6b-v3-ov".into(),
            device: "encoder:NPU".into(),
            rtfx: Some(12.34),
            segments,
        }
    }

    #[test]
    fn merge_interleaves_channels_by_start_time() {
        let me = vec![seg(Speaker::Me, 1.0, "a"), seg(Speaker::Me, 9.0, "c")];
        let others = vec![seg(Speaker::Others, 4.0, "b"), seg(Speaker::Others, 12.0, "d")];
        let texts: Vec<_> = merge(me, others).into_iter().map(|s| s.text).collect();
        assert_eq!(texts, ["a", "b", "c", "d"]);
    }

    #[test]
    fn merge_puts_me_first_on_ties_and_handles_one_empty_side() {
        let merged = merge(vec![seg(Speaker::Me, 2.0, "m")], vec![seg(Speaker::Others, 2.0, "o")]);
        assert_eq!(merged[0].speaker, Speaker::Me);
        assert_eq!(merge(Vec::new(), vec![seg(Speaker::Others, 0.0, "o")]).len(), 1);
        assert!(merge(Vec::new(), Vec::new()).is_empty());
    }

    #[test]
    fn timestamps_always_show_hours() {
        assert_eq!(timestamp(0.0), "00:00:00");
        assert_eq!(timestamp(83.9), "00:01:23");
        assert_eq!(timestamp(3725.0), "01:02:05");
        assert_eq!(timestamp(-3.0), "00:00:00");
    }

    #[test]
    fn markdown_has_a_header_and_one_line_per_segment() {
        let md = to_markdown(&transcript(vec![
            seg(Speaker::Me, 83.0, " Hello there. "),
            seg(Speaker::Others, 85.2, "Hi!"),
        ]));
        assert!(md.starts_with("# Transcript: 2026-10-19 12:16 · Teams\n"), "{md}");
        assert!(md.contains("- **Recording:** `2026-10-19_1216_Teams.opus`"));
        assert!(md.contains("- **Apps:** Teams"));
        assert!(md.contains("- **Duration:** 17:23"));
        assert!(md.contains("parakeet-tdt-0.6b-v3-ov on encoder:NPU, 12.3x real time"));
        assert!(md.contains("CC-BY-4.0"));
        assert!(md.contains("**[00:01:23] Me:** Hello there.\n"));
        assert!(md.contains("**[00:01:25] Others:** Hi!\n"));
        assert!(md.find("Me:").unwrap() < md.find("Others:").unwrap());
    }

    #[test]
    fn markdown_without_a_sidecar_uses_the_file_name() {
        let mut t = transcript(Vec::new());
        t.started = None;
        t.apps.clear();
        t.rtfx = None;
        let md = to_markdown(&t);
        assert!(md.starts_with("# Transcript: 2026-10-19_1216_Teams\n"), "{md}");
        assert!(!md.contains("**Apps:**") && !md.contains("**Date:**"));
        assert!(md.contains("on encoder:NPU\n"));
        assert!(md.contains("_No speech detected._"));
    }

    #[test]
    fn md_sits_next_to_the_voice_file_with_the_same_base_name() {
        assert_eq!(
            md_path(Path::new("D:\\rec\\2026\\10\\2026-10-23_1030_Teams.opus")),
            Path::new("D:\\rec\\2026\\10\\2026-10-23_1030_Teams.md")
        );
    }

    #[test]
    fn write_creates_the_md_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let opus = dir.path().join("a.opus");
        std::fs::write(&opus, b"OggS").unwrap();
        let path = write(&opus, &transcript(vec![seg(Speaker::Me, 0.0, "hi")]), false).unwrap();
        assert_eq!(path, dir.path().join("a.md"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("Me:** hi"));
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn an_existing_md_is_only_replaced_on_request() {
        let dir = tempfile::tempdir().unwrap();
        let opus = dir.path().join("a.opus");
        let md = dir.path().join("a.md");
        std::fs::write(&md, "mine").unwrap();
        assert!(write(&opus, &transcript(Vec::new()), false).is_err());
        assert_eq!(std::fs::read_to_string(&md).unwrap(), "mine");
        write(&opus, &transcript(Vec::new()), true).unwrap();
        assert!(std::fs::read_to_string(&md).unwrap().starts_with("# Transcript"));
    }

    #[test]
    fn segments_deserialize_from_the_helpers_json() {
        let s: Segment = serde_json::from_str(r#"{"speaker":"others","start":1.5,"end":2.0,"text":"x"}"#).unwrap();
        assert_eq!(s.speaker, Speaker::Others);
    }
}
