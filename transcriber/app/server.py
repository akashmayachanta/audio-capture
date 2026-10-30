"""FastAPI app: the page, the recordings list with transcript status, the
transcript of one recording, and starting/cancelling transcriptions."""
import logging
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel

from . import markdown, recordings, settings as settings_mod
from .engines import EngineCache
from .jobs import Jobs

STATIC = Path(__file__).parent / "static"
log = logging.getLogger("transcriber")


class TranscribeRequest(BaseModel):
    id: str
    model: str


class CancelRequest(BaseModel):
    job_id: int


def create_app(settings=None, jobs=None):
    s = settings or settings_mod.load()
    jobs = jobs or Jobs(EngineCache(s))
    app = FastAPI(title="MeetRec Transcripts", docs_url=None, redoc_url=None, openapi_url=None)
    allowed_hosts = {f"{h}:{s.port}" for h in ("127.0.0.1", "localhost", s.host)} | {"testserver"}

    @app.middleware("http")
    async def local_only(request: Request, call_next):
        # Blocks DNS rebinding: another site's page must not read transcripts.
        if request.headers.get("host", "") not in allowed_hosts:
            return JSONResponse({"detail": "forbidden host"}, status_code=403)
        return await call_next(request)

    def root():
        return s.recordings_root()

    def recording_path(rec_id):
        path = recordings.resolve(root(), rec_id)
        if path is None:
            raise HTTPException(404, "no such recording")
        return path

    @app.get("/")
    def index():
        return FileResponse(STATIC / "index.html", headers={"Cache-Control": "no-store"})

    @app.get("/api/state")
    def state():
        return {
            "recordings_dir": str(root()),
            "default_model": s.model,
            "models": [
                {"id": "bark", "label": "Underdog Bark 3.0 (English)"},
                {"id": "whistle", "label": f"Cactus Whistle ({s.language})"},
            ],
            "recordings": recordings.list_recordings(root()),
            "jobs": jobs.snapshot(),
        }

    @app.get("/api/transcript")
    def transcript(id: str):
        path = recording_path(id)
        md = Path(markdown.md_path(str(path)))
        if not md.is_file():
            raise HTTPException(404, "not transcribed yet")
        text = md.read_text(encoding="utf-8")
        return {"id": id, "markdown": text, **markdown.parse(text)}

    @app.post("/api/transcribe")
    def transcribe(req: TranscribeRequest):
        if req.model not in settings_mod.MODELS:
            raise HTTPException(400, f"unknown model {req.model!r}")
        job = jobs.submit(req.id, recording_path(req.id), req.model)
        return job.public()

    @app.post("/api/cancel")
    def cancel(req: CancelRequest):
        return {"cancelled": jobs.cancel(req.job_id)}

    return app
