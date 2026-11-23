"""`settings.toml` next to this package, plus where AudCap keeps its recordings."""
import os
import tomllib
from dataclasses import dataclass, fields
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SETTINGS_FILE = Path(os.environ.get("AUDCAP_TRANSCRIBER_SETTINGS", ROOT / "settings.toml"))
MODELS = ("bark", "whistle")


@dataclass
class Settings:
    recordings_dir: str = ""
    model: str = "whistle"
    device: str = "auto"
    language: str = "en"
    workers: int = 0
    cache_dir: str = ""
    host: str = "127.0.0.1"
    port: int = 8765

    def recordings_root(self):
        return Path(self.recordings_dir) if self.recordings_dir else audcap_output_dir()

    def whistle_workers(self):
        """Whistle processes run side by side; each uses about 2 cores. 0 = auto:
        a third of the logical cores, at most 4 (more than 4 got slower on a
        15 W laptop CPU)."""
        if self.workers > 0:
            return min(self.workers, 16)
        return max(1, min(4, (os.cpu_count() or 1) // 3))

    def cache_root(self):
        if self.cache_dir:
            return Path(self.cache_dir)
        return Path(os.environ.get("LOCALAPPDATA", Path.home())) / "AudCap" / "transcriber"


def load(path=SETTINGS_FILE):
    try:
        with open(path, "rb") as f:
            raw = tomllib.load(f)
    except FileNotFoundError:
        raw = {}
    known = {f.name for f in fields(Settings)}
    s = Settings(**{k: v for k, v in raw.items() if k in known})
    if s.model not in MODELS:
        s.model = "whistle"
    s.device = s.device.lower()
    s.language = s.language.lower()
    return s


def audcap_config_path():
    if env := os.environ.get("AUDCAP_CONFIG"):
        return Path(env)
    return Path(os.environ.get("APPDATA", Path.home())) / "AudCap" / "config.toml"


def audcap_output_dir(config_path=None):
    """AudCap's `output_dir`, or its default (Documents\\AudCap)."""
    try:
        with open(config_path or audcap_config_path(), "rb") as f:
            out = tomllib.load(f).get("output_dir")
        if out:
            return Path(out)
    except (OSError, tomllib.TOMLDecodeError):
        pass
    return Path.home() / "Documents" / "AudCap"
