import json
from datetime import datetime
from urllib.parse import urlsplit
from uuid import uuid4

from fastapi import APIRouter, HTTPException, Response
from app.api.schemas import PortalIn
from pydantic import BaseModel, StrictBool
from app.db.database import transaction, auto_refresh_enabled, set_auto_refresh
from app.worker import scheduler

router = APIRouter()


def portal(portal_id):
    with transaction() as db:
        row = db.execute("SELECT * FROM portals WHERE id=?", (portal_id,)).fetchone()
    if row is None:
        raise HTTPException(404, "Unknown portal")
    return row


def summary(row):
    return {**{key: row[key] for key in ('id', 'url', 'reason', 'state', 'lastSuccessAt')},
            'tenderCount': len(json.loads(row['snapshot']))}


@router.post('/portals', status_code=201)
async def add_portal(body: PortalIn):
    parts = urlsplit(body.url)
    if parts.scheme not in ('http', 'https') or not parts.hostname or parts.username:
        raise HTTPException(422, 'Expected an HTTP portal URL')
    with transaction() as db:
        existing = db.execute("SELECT id FROM portals WHERE url=?", (body.url,)).fetchone()
        if existing:
            return dict(existing)
        key = uuid4().hex
        db.execute("INSERT INTO portals(id,url,state,reason) VALUES(?,?,'integrating','Learning the listing layout')", (key, body.url))
    scheduler.schedule(key)
    return {'id': key}


@router.get('/portals')
def list_portals():
    with transaction() as db:
        return [summary(row) for row in db.execute("SELECT * FROM portals")]


@router.get('/portals/{portal_id}')
def get_portal(portal_id: str):
    return summary(portal(portal_id))


@router.delete('/portals/{portal_id}', status_code=204)
async def delete_portal(portal_id: str):
    portal(portal_id)
    scheduler.cancel(portal_id)
    with transaction() as db:
        db.execute("DELETE FROM portals WHERE id=?", (portal_id,))
        db.execute("DELETE FROM runs WHERE portal=?", (portal_id,))
    return Response(status_code=204)


@router.post('/portals/{portal_id}/refresh', status_code=202)
async def refresh_portal(portal_id: str):
    portal(portal_id)
    return {'runId': scheduler.schedule(portal_id)}


@router.get('/portals/{portal_id}/tenders')
def list_tenders(portal_id: str, changedSince: datetime | None = None):
    rows = json.loads(portal(portal_id)['snapshot'])
    if changedSince is None:
        return rows
    if changedSince.tzinfo is None:
        raise HTTPException(422, 'Include a timezone offset')
    return [row for row in rows if datetime.fromisoformat(row['lastChangedAt']) > changedSince]


@router.get('/portals/{portal_id}/runs')
def list_runs(portal_id: str):
    portal(portal_id)
    with transaction() as db:
        return [dict(row) for row in db.execute(
            "SELECT id,kind,startedAt,finishedAt,result FROM runs WHERE portal=? ORDER BY startedAt DESC", (portal_id,))]

class RefreshPreference(BaseModel):
    enabled: StrictBool


@router.get('/settings/auto-refresh')
def refresh_preference():
    return {'enabled': auto_refresh_enabled()}


@router.post('/settings/auto-refresh')
async def update_refresh_preference(body: RefreshPreference):
    set_auto_refresh(body.enabled)
    return {'enabled': body.enabled}
