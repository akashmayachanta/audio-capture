"""`uv run python -m app [--no-browser]`: serve the transcripts page on localhost."""
import argparse
import logging
import os
import threading
import warnings
import webbrowser

import uvicorn

from . import settings as settings_mod
from .server import build


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--no-browser", action="store_true", help="do not open the page")
    args = ap.parse_args()

    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    # transformers nags about max_length on every Parakeet generate(); the default fits the audio.
    warnings.filterwarnings("ignore", message=".*max_length.*")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")

    s = settings_mod.load()
    url = f"http://{s.host}:{s.port}/"
    logging.getLogger("transcriber").info("recordings: %s | models: %s | %s", s.recordings_root(), s.cache_root(), url)
    if not args.no_browser:
        threading.Timer(1.5, webbrowser.open, [url]).start()
    uvicorn.run(build(s), host=s.host, port=s.port, log_level="warning")


if __name__ == "__main__":
    main()
