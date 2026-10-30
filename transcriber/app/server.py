"""FastAPI app: the page, the recordings list with transcript status, the
transcript of one recording, loading a model and starting/cancelling
transcriptions."""
import logging
from pathlib import Path

from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import FileResponse, JSONResponse
from pydantic import BaseModel

from . import markdown, recordings
from .engines import CATALOG, EngineCache
from .jobs import Jobs
from .store import Store

STATIC = Path(__file__).parent / "static"
log = logging.getLogger("transcriber")


class TranscribeRequest(BaseModel):
    id: str


class LoadRequest(BaseModel):
    model: str


class CancelRequest(BaseModel):
    job_id: int


def build(settings):
    """The real wiring: SQLite store in the cache folder, and the model loaded
    last (or settings.toml's) starts loading right away."""
    store = Store(settings.cache_root() / "transcriber.db", CATALOG)
    engines = EngineCache(settings, on_loaded=lambda model: store.set_pref("model", model))
    if model := store.startup_model(settings.model):
        engines.load_async(model)
    return create_app(settings, engines, store)


def create_app(s, engines, store, jobs=None):
    jobs = jobs or Jobs(engines)
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

    def models():
        out = store.models()
        for m in out:
            if m["id"] == "whistle":
                m["description"] += f" Language: {s.language} (settings.toml)."
        return out

    @app.get("/")
    def index():
        return FileResponse(STATIC / "index.html", headers={"Cache-Control": "no-store"})

    @app.get("/api/state")
    def state():
        return {
            "recordings_dir": str(root()),
            "models": models(),
            "engine": engines.status(),
            "recordings": recordings.list_recordings(root()),
            "jobs": jobs.snapshot(),
        }

    @app.post("/api/load")
    def load(req: LoadRequest):
        if req.model not in {m["id"] for m in store.models()}:
            raise HTTPException(400, f"unknown model {req.model!r}")
        engines.load_async(req.model)
        return engines.status()

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
        if engines.status()["active"] is None:
            raise HTTPException(409, "load a model first (settings)")
        return jobs.submit(req.id, recording_path(req.id)).public()

    @app.post("/api/cancel")
    def cancel(req: CancelRequest):
        return {"cancelled": jobs.cancel(req.job_id)}

    return app
