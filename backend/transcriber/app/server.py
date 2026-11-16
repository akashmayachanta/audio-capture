"""FastAPI app, API only (the MeetRec window is the UI): recordings with
transcript status, loading a model and starting/cancelling transcriptions, and stopping the server.
The window reads transcripts and audio from meetrec.exe itself."""
import logging

from fastapi import FastAPI, HTTPException, Request
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from pydantic import BaseModel
from starlette.concurrency import run_in_threadpool

from . import dictation, recordings
from .engines import CATALOG, EngineCache
from .jobs import Jobs
from .store import Store

# The MeetRec window (WebView2 custom protocol) calls this API cross-origin.
APP_ORIGIN = "http://meetrec.localhost"
MAX_DICTATION_BYTES = 20 * 1024 * 1024
log = logging.getLogger("transcriber")


class TranscribeRequest(BaseModel):
    id: str


class LoadRequest(BaseModel):
    model: str


class CancelRequest(BaseModel):
    job_id: int


def build(settings, shutdown=lambda: None):
    """The real wiring: SQLite store in the cache folder, and the model loaded
    last (or settings.toml's) starts loading right away."""
    store = Store(settings.cache_root() / "transcriber.db", CATALOG)
    engines = EngineCache(settings, on_loaded=lambda model: store.set_pref("model", model))
    if model := store.startup_model(settings.model):
        engines.load_async(model)
    return create_app(settings, engines, store, shutdown=shutdown)


def create_app(s, engines, store, jobs=None, shutdown=lambda: None):
    jobs = jobs or Jobs(engines)
    app = FastAPI(title="MeetRec Transcripts", docs_url=None, redoc_url=None, openapi_url=None)
    app.state.engines = engines
    allowed_hosts = {f"{h}:{s.port}" for h in ("127.0.0.1", "localhost", s.host)} | {"testserver"}

    @app.middleware("http")
    async def local_only(request: Request, call_next):
        # Blocks DNS rebinding: another site's page must not read transcripts.
        if request.headers.get("host", "") not in allowed_hosts:
            return JSONResponse({"detail": "forbidden host"}, status_code=403)
        return await call_next(request)

    # Added after the host check, so it runs first (outermost) and answers preflights.
    app.add_middleware(CORSMiddleware, allow_origins=[APP_ORIGIN], allow_methods=["GET", "POST"],
                       allow_headers=["Content-Type"])

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
        # Loading frees the current model, which would fail the transcription using it.
        if any(j["state"] in ("queued", "running") for j in jobs.snapshot()):
            raise HTTPException(409, "a transcription is running; wait for it to finish or cancel it")
        engines.load_async(req.model)
        return engines.status()

    @app.post("/api/transcribe")
    def transcribe(req: TranscribeRequest):
        if engines.status()["active"] is None:
            raise HTTPException(409, "load a model first (settings)")
        return jobs.submit(req.id, recording_path(req.id)).public()

    @app.post("/api/cancel")
    def cancel(req: CancelRequest):
        return {"cancelled": jobs.cancel(req.job_id)}

    @app.get("/api/health")
    def health():
        """Cheap liveness check for the tray (state lists every recording)."""
        return {"ok": True}

    @app.post("/api/dictate")
    async def dictate(request: Request):
        """Body = a dictation clip (Ogg Opus). Not a job: the answer is the text."""
        if int(request.headers.get("content-length") or 0) > MAX_DICTATION_BYTES:
            raise HTTPException(413, "clip too large")
        data = await request.body()
        if not data:
            raise HTTPException(400, "no audio")
        try:
            return await run_in_threadpool(dictation.transcribe, engines, data)
        except dictation.NoModel as e:
            raise HTTPException(409, str(e)) from None
        except dictation.Loading as e:
            raise HTTPException(503, str(e)) from None
        except dictation.BadAudio as e:
            raise HTTPException(400, str(e)) from None
        except dictation.ModelFailed as e:
            raise HTTPException(500, str(e)) from None

    @app.post("/api/shutdown")
    def stop():
        """The window's Stop button. A running transcription is abandoned."""
        log.info("shutdown requested")
        shutdown()
        return {"stopping": True}

    return app
