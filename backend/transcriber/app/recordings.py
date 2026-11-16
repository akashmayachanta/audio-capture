"""Recordings under MeetRec's output folder (`YYYY/MM/*.opus` + `.json` sidecar)
and whether each has a transcript next to it."""
import json
from datetime import datetime
from pathlib import Path

from .audio import speakers_for
from .markdown import md_path


def read_sidecar(opus):
    try:
        with open(Path(opus).with_suffix(".json"), encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def started(meta, opus):
    """Start time from the sidecar, else from the `YYYY-MM-DD_HHMM_` file name."""
    if s := meta.get("start"):
        try:
            return datetime.fromisoformat(s).replace(tzinfo=None)
        except ValueError:
            pass
    try:
        return datetime.strptime(Path(opus).stem[:15], "%Y-%m-%d_%H%M")
    except ValueError:
        return None


def app_of(meta, opus):
    apps = meta.get("apps") or []
    if apps:
        return ", ".join(apps)
    parts = Path(opus).stem.split("_", 2)
    return parts[2] if len(parts) == 3 else ""


def describe(root, opus):
    meta = read_sidecar(opus)
    when = started(meta, opus)
    md = Path(md_path(str(opus)))
    return {
        "id": opus.relative_to(root).as_posix(),
        "file": opus.name,
        "started": when.strftime("%Y-%m-%d %H:%M") if when else "",
        "sort": when.isoformat() if when else opus.stem,
        "app": app_of(meta, opus),
        "duration_secs": meta.get("duration_secs"),
        "status": meta.get("status", ""),
        # Speaker per channel, in channel order (["me", "others"] for stereo).
        "speakers": speakers_for(meta.get("channels"), 1 if "mono" in (meta.get("channels") or {}) else 2),
        "transcribed": md.is_file(),
        "transcript_mtime": md.stat().st_mtime if md.is_file() else None,
    }


def list_recordings(root):
    """Newest first. `.opus.part` files (still recording) are skipped by the glob."""
    root = Path(root)
    if not root.is_dir():
        return []
    items = [describe(root, p) for p in root.rglob("*.opus") if p.is_file()]
    return sorted(items, key=lambda r: r["sort"], reverse=True)


def resolve(root, rec_id):
    """The recording's path, or None if `rec_id` is not an .opus file inside `root`."""
    root = Path(root).resolve()
    path = (root / rec_id).resolve()
    if path.suffix != ".opus" or not path.is_file() or not path.is_relative_to(root):
        return None
    return path
