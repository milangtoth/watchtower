"""GET / : the operator's morning page."""
from pathlib import Path

from fastapi import APIRouter
from fastapi.responses import HTMLResponse

router = APIRouter()
_INDEX = Path(__file__).parent / "templates" / "index.html"


@router.get("/", response_class=HTMLResponse)
def dashboard() -> str:
    return _INDEX.read_text(encoding="utf-8")
