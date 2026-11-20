from contextlib import contextmanager
from datetime import datetime, timezone
import sqlite3

from app import config
from app.normalize.dates import OUTPUT_ZONE


def now():
    return datetime.now(OUTPUT_ZONE).isoformat()


@contextmanager
def transaction():
    db = sqlite3.connect(config.DB_PATH)
    db.row_factory = sqlite3.Row
    try:
        with db:
            yield db
    finally:
        db.close()


def init():
    config.FILES_DIR.mkdir(parents=True, exist_ok=True)
    with transaction() as db:
        db.executescript("""
            CREATE TABLE IF NOT EXISTS preferences (name TEXT PRIMARY KEY, enabled INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS portals (
                id TEXT PRIMARY KEY, url TEXT UNIQUE, state TEXT, reason TEXT,
                lastSuccessAt TEXT, recipe TEXT, snapshot TEXT DEFAULT '[]'
            );
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY, portal TEXT, kind TEXT, startedAt TEXT,
                finishedAt TEXT, result TEXT
            );
            CREATE INDEX IF NOT EXISTS history ON runs(portal, startedAt);
        """)
        db.execute("UPDATE runs SET finishedAt=?, result='failed' WHERE finishedAt IS NULL", (now(),))

def auto_refresh_enabled():
    with transaction() as db:
        row = db.execute("SELECT enabled FROM preferences WHERE name='auto_refresh'").fetchone()
    return bool(row and row['enabled'])


def set_auto_refresh(enabled):
    with transaction() as db:
        db.execute("INSERT OR REPLACE INTO preferences VALUES('auto_refresh', ?)", (enabled,))
