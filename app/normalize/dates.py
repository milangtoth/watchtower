"""Translate source wall times to a single output zone, preserving the instant."""
import os
import re
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from urllib.parse import urlsplit
from zoneinfo import ZoneInfo

from dateutil.parser import isoparse

OUTPUT_ZONE = ZoneInfo(os.getenv("WATCHTOWER_OUTPUT_TIMEZONE", "Etc/GMT-1"))


def parse_source(value):
    if not value:
        return None
    for parser in (isoparse, parsedate_to_datetime):
        try:
            return parser(value)
        except (ValueError, TypeError, OverflowError):
            pass
    for fmt in ("%d-%m-%Y %H:%M", "%d-%m-%Y", "%d.%m.%Y %H:%M"):
        try:
            return datetime.strptime(value, fmt)
        except ValueError:
            continue
    return None


def publication_date(value):
    parsed = parse_source(value)
    return parsed.date().isoformat() if parsed else None


def deadline(value, portal_url, zone_name=None):
    if not value or not re.search(r'\d{1,2}:\d{2}', value):
        return None
    parsed = parse_source(value)
    if parsed is None:
        return None
    if parsed.tzinfo is None:
        if not zone_name and (urlsplit(portal_url).hostname or '').endswith('.nl'):
            zone_name = 'Europe/Amsterdam'
        if not zone_name:
            return None
        zone = ZoneInfo(zone_name)
        first = parsed.replace(tzinfo=zone, fold=0)
        second = parsed.replace(tzinfo=zone, fold=1)
        # Clock changes create missing or repeated wall times. Both need source evidence.
        if first.utcoffset() != second.utcoffset():
            return None
        parsed = first
    return parsed.astimezone(OUTPUT_ZONE).isoformat()
