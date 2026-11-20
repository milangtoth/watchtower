"""SQLite connection and schema: portals, runs, tenders, documents, recipes."""
import sqlite3

from app import config


def connect() -> sqlite3.Connection:
    conn = sqlite3.connect(config.DB_PATH, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    return conn


def init() -> None:
    config.DATA_DIR.mkdir(parents=True, exist_ok=True)
    config.FILES_DIR.mkdir(parents=True, exist_ok=True)
    # TODO: CREATE TABLE IF NOT EXISTS ...
