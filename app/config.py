"""Settings read from environment variables. Every model key is listed in .env.example and the README."""
import os
from pathlib import Path

DATA_DIR = Path(os.environ.get("WATCHTOWER_DATA_DIR", "./data"))
DB_PATH = DATA_DIR / "watchtower.db"
FILES_DIR = DATA_DIR / "files"

OPENAI_API_KEY = os.environ.get("OPENAI_API_KEY", "")
MODEL = os.environ.get("WATCHTOWER_MODEL", "gpt-4.1-mini")

READ_INTERVAL_SECONDS = int(os.environ.get("WATCHTOWER_READ_INTERVAL", "900"))
MAX_REQUESTS_PER_SECOND_PER_PORTAL = 1.0  # brief: at most 1 req/s per portal
MAX_FILE_BYTES = 250 * 1024 * 1024  # brief: files over 250 MB may be left out

TYPESAFE_API_KEY = os.environ.get("TYPESAFE_API_KEY", "")
JEV_MODEL = os.environ.get("WATCHTOWER_JEV_MODEL", "jev-latest")
