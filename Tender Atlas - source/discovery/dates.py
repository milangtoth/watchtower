from datetime import date, datetime
import re

from dateutil.parser import parse


def clean_date(value):
    """Return the publisher's calendar day as ISO 8601, or blank if unknown."""
    if isinstance(value, datetime):
        return value.date().isoformat()
    if isinstance(value, date):
        return value.isoformat()
    if not isinstance(value, str) or not value.strip():
        return ""
    text = value.strip()
    try:
        if re.match(r"^\d{4}-\d{2}-\d{2}(?:$|[T +Z-])", text):
            return date.fromisoformat(text[:10]).isoformat()
        if not re.search(r"\b\d{4}\b", text):
            return ""
        # Different default days reveal inputs that omitted the day.
        first = parse(text, dayfirst=True, default=datetime(2000, 1, 1))
        second = parse(text, dayfirst=True, default=datetime(2000, 2, 2))
        return first.date().isoformat() if first.date() == second.date() else ""
    except (ValueError, OverflowError):
        return ""

