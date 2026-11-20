"""Portal registry and auditable refresh runs."""
import json
import uuid
from urllib.parse import urlsplit

import indexdb as db
from discovery import access, service
from discovery.public import public_url


def setup():
    with db.conn() as c:
        c.executescript("""
        CREATE TABLE IF NOT EXISTS portals(
            id TEXT PRIMARY KEY, address TEXT UNIQUE NOT NULL, status TEXT NOT NULL,
            created TEXT NOT NULL, checked TEXT, delivered INTEGER NOT NULL DEFAULT 0,
            message TEXT NOT NULL DEFAULT '');
        CREATE TABLE IF NOT EXISTS portal_runs(
            id INTEGER PRIMARY KEY AUTOINCREMENT, portal_id TEXT NOT NULL,
            started TEXT NOT NULL, finished TEXT, action TEXT NOT NULL,
            status TEXT NOT NULL, summary TEXT NOT NULL DEFAULT '',
            FOREIGN KEY(portal_id) REFERENCES portals(id) ON DELETE CASCADE);
        CREATE TABLE IF NOT EXISTS portal_tenders(
            portal_id TEXT NOT NULL, notice_id INTEGER NOT NULL,
            PRIMARY KEY(portal_id,notice_id),
            FOREIGN KEY(portal_id) REFERENCES portals(id) ON DELETE CASCADE,
            FOREIGN KEY(notice_id) REFERENCES notices(id) ON DELETE CASCADE);
        """)
        c.execute("PRAGMA foreign_keys=ON")


def _entry(address):
    return str(public_url(address))


def _dict(row):
    return dict(row) if row else None


def list_portals():
    with db.conn() as c:
        return [dict(r) for r in c.execute(
            "SELECT p.*, (SELECT COUNT(*) FROM portal_tenders t WHERE t.portal_id=p.id) AS tender_count "
            "FROM portals p ORDER BY created DESC")]


def get_portal(portal_id):
    with db.conn() as c:
        row = c.execute(
            "SELECT p.*, (SELECT COUNT(*) FROM portal_tenders t WHERE t.portal_id=p.id) AS tender_count "
            "FROM portals p WHERE id=?", (portal_id,)).fetchone()
    if not row:
        raise KeyError("Portal not found.")
    return dict(row)


def add(address):
    address = _entry(address)
    with db.conn() as c:
        old = c.execute("SELECT id FROM portals WHERE address=?", (address,)).fetchone()
        if old:
            return get_portal(old["id"])
        portal_id = str(uuid.uuid4())
        c.execute("INSERT INTO portals(id,address,status,created) VALUES(?,?,'integrating',?)",
                  (portal_id, address, db.now()))
        c.execute("INSERT INTO portal_runs(portal_id,started,action,status) VALUES(?,?, 'integrate','running')",
                  (portal_id, db.now()))
    refresh(portal_id, action="integrate")
    return get_portal(portal_id)


def refresh(portal_id, action="refresh"):
    portal = get_portal(portal_id)
    started = db.now()
    with db.conn() as c:
        run_id = c.execute("INSERT INTO portal_runs(portal_id,started,action,status) VALUES(?,?,?,'running')",
                           (portal_id, started, action)).lastrowid
        c.execute("UPDATE portals SET status='healing',message='' WHERE id=?", (portal_id,))
    try:
        result = service.preview(portal["address"], refresh=True)
        if result.get("excluded"):
            raise ValueError(result["warning"])
        if result.get("warning") and not result.get("sample"):
            raise ValueError(result["warning"])
        sample = result.get("sample") or {}
        if sample.get("verified"):
            imported = service.add_to_register(portal["address"])
            urls = [row["url"] for row in sample.get("rows", [])]
            with db.conn() as c:
                c.execute("DELETE FROM portal_tenders WHERE portal_id=?", (portal_id,))
                ids = [r[0] for url in urls for r in c.execute(
                    "SELECT id FROM notices WHERE source='portal' AND url=?", (url,))]
                c.executemany("INSERT OR IGNORE INTO portal_tenders VALUES(?,?)",
                              [(portal_id, row["id"]) for row in ids])
            count = imported["count"]
        else:
            raise ValueError(result.get("warning") or "Portal sample could not be verified.")
        message = ""
        status = "serving"
        summary = f"Verified and imported {count} notices."
        delivered = 1
    except Exception as exc:
        message = str(exc)[:400] or type(exc).__name__
        status = "failed"
        summary = message
        delivered = 0
    finished = db.now()
    with db.conn() as c:
        c.execute("UPDATE portals SET status=?,checked=?,delivered=?,message=? WHERE id=?",
                  (status, finished, delivered, message, portal_id))
        c.execute("UPDATE portal_runs SET finished=?,status=?,summary=? WHERE id=?",
                  (finished, status, summary, run_id))
    return get_portal(portal_id)


def remove(portal_id):
    get_portal(portal_id)
    with db.conn() as c:
        c.execute("DELETE FROM portal_tenders WHERE portal_id=?", (portal_id,))
        c.execute("DELETE FROM portal_runs WHERE portal_id=?", (portal_id,))
        c.execute("DELETE FROM portals WHERE id=?", (portal_id,))
    return {"deleted": True, "id": portal_id}


def tenders(portal_id):
    get_portal(portal_id)
    with db.conn() as c:
        return [dict(r) for r in c.execute(
            "SELECT n.id AS local_id,n.source,n.ref AS identifier,n.url AS address,n.title,n.buyer,"
            "n.published AS publication_date,n.deadline,n.stage AS status,n.origin,n.description,"
            "n.amount,n.currency,n.raw FROM portal_tenders t JOIN notices n ON n.id=t.notice_id "
            "WHERE t.portal_id=? ORDER BY n.published DESC,n.id DESC", (portal_id,))]


def runs(portal_id):
    get_portal(portal_id)
    with db.conn() as c:
        return [dict(r) for r in c.execute(
            "SELECT * FROM portal_runs WHERE portal_id=? ORDER BY id DESC", (portal_id,))]

