"""The transcript Markdown file next to a recording (`…_Teams.opus` -> `…_Teams.md`),
in the same layout AudCap has always written, and parsing it back for the UI."""
import os
import re
from dataclasses import dataclass, field

LABELS = {"me": "Me", "others": "Others"}
LINE = re.compile(r"^\*\*\[(\d+):(\d\d):(\d\d)\] (Me|Others):\*\* ?(.*)$")
FIELD = re.compile(r"^- \*\*(.+?):\*\* (.*)$")


@dataclass
class Segment:
    speaker: str  # "me" | "others"
    start: float
    end: float
    text: str


@dataclass
class Transcript:
    audio_file: str
    title: str
    started: str = ""  # "2026-10-19 12:16"
    apps: list = field(default_factory=list)
    duration_secs: float = 0.0
    model: str = ""
    model_credit: str = ""
    device: str = ""
    rtfx: float | None = None
    segments: list = field(default_factory=list)


def merge(*channels):
    """Interleaves segments by start time; equal starts keep "me" first."""
    order = {"me": 0, "others": 1}
    return sorted((s for ch in channels for s in ch), key=lambda s: (s.start, order.get(s.speaker, 2)))


def timestamp(secs):
    s = int(max(0.0, secs))
    return f"{s // 3600:02}:{s // 60 % 60:02}:{s % 60:02}"


def duration(secs):
    s = int(round(secs))
    return f"{s // 3600}:{s // 60 % 60:02}:{s % 60:02}" if s >= 3600 else f"{s // 60}:{s % 60:02}"


def to_markdown(t):
    out = [f"# Transcript: {t.title}", ""]
    out.append(f"- **Recording:** `{t.audio_file}`")
    if t.started:
        out.append(f"- **Date:** {t.started}")
    if t.apps:
        out.append(f"- **Apps:** {', '.join(t.apps)}")
    out.append(f"- **Duration:** {duration(t.duration_secs)}")
    speed = f", {t.rtfx:.1f}x real time" if t.rtfx else ""
    out.append(f"- **Transcribed with:** {t.model} on {t.device}{speed}")
    if t.model_credit:
        out.append(f"- **Model:** {t.model_credit}")
    out += ["", "---", ""]
    if not t.segments:
        out += ["_No speech detected._", ""]
    for s in t.segments:
        out += [f"**[{timestamp(s.start)}] {LABELS[s.speaker]}:** {' '.join(s.text.split())}", ""]
    return "\n".join(out)


def md_path(opus_path):
    return os.path.splitext(opus_path)[0] + ".md"


def write(opus_path, t):
    """Atomic: a crash never leaves a torn transcript."""
    path = md_path(opus_path)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="\n") as f:
        f.write(to_markdown(t))
    os.replace(tmp, path)
    return path


def parse(text):
    """-> {"title", "fields": {name: value}, "segments": [{speaker, start, text}]}.
    Lines that are not transcript lines are ignored, so hand edits survive."""
    title, fields, segments = "", {}, []
    for line in text.splitlines():
        if line.startswith("# ") and not title:
            title = line[2:].removeprefix("Transcript: ").strip()
        elif m := FIELD.match(line):
            fields[m.group(1)] = m.group(2).strip("`")
        elif m := LINE.match(line):
            h, mi, s, who, said = m.groups()
            segments.append({"speaker": who.lower(), "start": int(h) * 3600 + int(mi) * 60 + int(s), "text": said})
    return {"title": title, "fields": fields, "segments": segments}
