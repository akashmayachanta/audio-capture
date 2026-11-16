"""Small SQLite file in the cache folder: which models the settings dialog lists
(`models`, set `enabled` to 0 to hide one) and the model loaded last
(`prefs.model`), which is loaded again when the app starts."""
import sqlite3
import threading
from contextlib import contextmanager
from pathlib import Path

SCHEMA = """
CREATE TABLE IF NOT EXISTS models (
    id          TEXT PRIMARY KEY,
    label       TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    enabled     INTEGER NOT NULL DEFAULT 1,
    sort        INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS prefs (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"""


class Store:
    def __init__(self, path, catalog):
        """`catalog`: [{"id", "label", "description"}] of the models the code can
        run, in display order. New ones are added; label, description and order
        follow the code, while `enabled` stays as the user left it. Rows for
        models the code no longer has are never listed."""
        self.path = str(path)
        self.known = [m["id"] for m in catalog]
        self.lock = threading.Lock()
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        with self._db() as c:
            c.executescript(SCHEMA)
            for i, m in enumerate(catalog):
                c.execute(
                    "INSERT INTO models (id, label, description, sort) VALUES (?, ?, ?, ?) "
                    "ON CONFLICT(id) DO UPDATE SET label = excluded.label, description = excluded.description, "
                    "sort = excluded.sort",
                    (m["id"], m["label"], m.get("description", ""), i))

    @contextmanager
    def _db(self):
        with self.lock:
            conn = sqlite3.connect(self.path)
            conn.row_factory = sqlite3.Row
            try:
                with conn:  # commits, or rolls back on error
                    yield conn
            finally:
                conn.close()

    def models(self):
        """Enabled models the code knows, in display order."""
        with self._db() as c:
            rows = c.execute("SELECT id, label, description FROM models WHERE enabled = 1 ORDER BY sort, id").fetchall()
        return [dict(r) for r in rows if r["id"] in self.known]

    def get_pref(self, key, default=None):
        with self._db() as c:
            row = c.execute("SELECT value FROM prefs WHERE key = ?", (key,)).fetchone()
        return row["value"] if row else default

    def set_pref(self, key, value):
        with self._db() as c:
            c.execute("INSERT INTO prefs (key, value) VALUES (?, ?) "
                      "ON CONFLICT(key) DO UPDATE SET value = excluded.value", (key, value))

    def startup_model(self, fallback):
        """The model loaded last, else `fallback` (settings.toml), else the first listed."""
        ids = [m["id"] for m in self.models()]
        for candidate in (self.get_pref("model"), fallback):
            if candidate in ids:
                return candidate
        return ids[0] if ids else None
