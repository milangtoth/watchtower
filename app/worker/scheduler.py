import asyncio
import json
from uuid import uuid4

import httpx

from app import config
from app.db.database import now, transaction, auto_refresh_enabled
from app.fetch.http import Reader
from app.recipes.discovery import discover
from app.recipes.learn import verify_sample
from app.worker.runner import read

_active = {}
_task = None


def record(portal, kind):
    run = uuid4().hex
    with transaction() as db:
        db.execute('INSERT INTO runs VALUES(?,?,?,?,NULL,NULL)', (run, portal, kind, now()))
    return run


def finish(run, result):
    with transaction() as db:
        db.execute('UPDATE runs SET finishedAt=?,result=? WHERE id=?', (now(), result, run))


def state(portal, value, reason):
    with transaction() as db:
        db.execute('UPDATE portals SET state=?,reason=? WHERE id=?', (value, reason, portal))


async def attach(reader, url):
    recipe, first_page = await discover(reader, url)
    records = await read(reader, recipe, first_page)
    if config.OPENAI_API_KEY:
        await verify_sample(reader, records)
    return recipe, records


async def crawl(portal, run):
    with transaction() as db:
        row = db.execute('SELECT * FROM portals WHERE id=?', (portal,)).fetchone()
    recipe = row['recipe']
    try:
        async with httpx.AsyncClient(timeout=30, trust_env=False,
                                     limits=httpx.Limits(max_keepalive_connections=0)) as client:
            reader = Reader(client)
            if recipe:
                try:
                    latest = await read(reader, recipe)
                except Exception as error:
                    if any(word in str(error).lower() for word in ('login', 'captcha', 'denies public')):
                        raise
                    finish(run, 'failed')
                    state(portal, 'healing', 'Saved recipe failed. Rediscovering from the landing page.')
                    run = record(portal, 'repair')
                    recipe, latest = await attach(reader, row['url'])
            else:
                recipe, latest = await attach(reader, row['url'])
        previous = {item['id'] or item['url']: item for item in json.loads(row['snapshot'])}
        changed = now()
        for key, old in previous.items():
            if key not in latest:
                latest[key] = {k: v for k, v in old.items() if k != 'lastChangedAt'}
                if old['status'] == 'open':
                    latest[key]['status'] = 'closed'
        for key, item in latest.items():
            old = previous.get(key, {})
            same = item == {k: v for k, v in old.items() if k != 'lastChangedAt'}
            item['lastChangedAt'] = old['lastChangedAt'] if same else changed
        with transaction() as db:
            db.execute("""UPDATE portals SET recipe=?, snapshot=?, lastSuccessAt=?,
                          state='serving', reason='Tender source read successfully' WHERE id=?""",
                       (recipe, json.dumps(list(latest.values())), changed, portal))
        finish(run, 'ok')
    except asyncio.CancelledError:
        finish(run, 'failed')
        raise
    except Exception as error:
        state(portal, 'failed', str(error)[:350] or 'Portal read failed')
        finish(run, 'failed')
    finally:
        _active.pop(portal, None)


def schedule(portal):
    if portal in _active:
        return _active[portal][0]
    run = record(portal, 'read')
    _active[portal] = run, asyncio.create_task(crawl(portal, run))
    return run


def cancel(portal):
    if portal in _active:
        _active[portal][1].cancel()


async def loop():
    while True:
        if not auto_refresh_enabled():
            await asyncio.sleep(5)
            continue
        with transaction() as db:
            due = db.execute("""
                SELECT p.id FROM portals p LEFT JOIN runs r ON p.id=r.portal
                GROUP BY p.id HAVING MAX(r.startedAt) IS NULL
                OR (julianday('now') - julianday(MAX(r.startedAt))) * 86400 >= ?
            """, (config.READ_INTERVAL_SECONDS,)).fetchall()
        for item in due:
            schedule(item['id'])
        await asyncio.sleep(5)


def start():
    global _task
    _task = asyncio.create_task(loop())


async def stop():
    tasks = [task for _, task in _active.values()]
    if _task:
        tasks.append(_task)
    for task in tasks:
        task.cancel()
    await asyncio.gather(*tasks, return_exceptions=True)
    _active.clear()
