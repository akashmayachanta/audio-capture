"""`uv run python -m app`: serve the transcription API for the MeetRec window on localhost."""
import logging
import os
import warnings

import uvicorn

from . import settings as settings_mod
from .server import build


def main():
    os.environ.setdefault("HF_HUB_DISABLE_SYMLINKS_WARNING", "1")
    # transformers nags about max_length on every Parakeet generate(); the default fits the audio.
    warnings.filterwarnings("ignore", message=".*max_length.*")
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")

    s = settings_mod.load()
    url = f"http://{s.host}:{s.port}/"
    logging.getLogger("transcriber").info("recordings: %s | models: %s | API on %s", s.recordings_root(), s.cache_root(), url)
    uvicorn.run(build(s), host=s.host, port=s.port, log_level="warning")


if __name__ == "__main__":
    main()
