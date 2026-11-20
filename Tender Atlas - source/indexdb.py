import hashlib
import json
import re
import sqlite3
import threading
import time
import zlib
from contextlib import contextmanager
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from feeds import SOURCES, COLLECTORS, DAY
import provenance
from discovery import access
from discovery.dates import clean_date

DB = None
CACHE = None
GUARD = threading.Lock()
STOP = threading.Event()
FIELDS = "ref title buyer country stage published url origin description deadline cpv amount currency value_type process supplier".split()
COLUMNS = ", ".join(FIELDS)
TRACE_JOIN = " LEFT JOIN source_locations USING(source,ref) LEFT JOIN source_documents USING(document_key) "
TRACE_COLUMNS = "document_json,locator_json"
LIST_COLUMNS = "id,source,ref,title,buyer,country,stage,published,deadline,amount,currency,value_type"
ORDER = {"recent":"published DESC,((id*2654435761)%2147483647) DESC","old":"published ASC,id ASC","deadline":"(deadline=''),deadline ASC,id DESC"}
def now():
    return datetime.now(timezone.utc).isoformat(timespec="seconds")
@contextmanager
def conn():
    db = sqlite3.connect(DB,timeout=45)
    db.row_factory = sqlite3.Row
    try:
        with db:
            yield db
    finally:
        db.close()

def setup(path):
    global DB, CACHE
    DB=Path(path)
    DB.parent.mkdir(parents=True,exist_ok=True)
    CACHE=DB.parent/"downloads"
    with conn() as c:
        c.executescript("""
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS notices(id INTEGER PRIMARY KEY,source TEXT NOT NULL,ref TEXT NOT NULL,title TEXT NOT NULL,buyer TEXT,country TEXT,stage TEXT,published TEXT,url TEXT,origin TEXT,description TEXT,deadline TEXT,cpv TEXT,amount TEXT,currency TEXT,value_type TEXT,process TEXT,supplier TEXT,raw BLOB NOT NULL,retrieved TEXT,UNIQUE(source,ref));
        CREATE INDEX IF NOT EXISTS idx_notice_date ON notices(published DESC,id DESC);
        CREATE INDEX IF NOT EXISTS idx_notice_ties ON notices(published DESC,((id*2654435761)%2147483647) DESC);
        CREATE INDEX IF NOT EXISTS idx_notice_geo ON notices(country,published DESC);
        CREATE INDEX IF NOT EXISTS idx_notice_stage ON notices(stage,published DESC);
        CREATE INDEX IF NOT EXISTS idx_notice_source ON notices(source,published DESC);
        CREATE VIRTUAL TABLE IF NOT EXISTS words USING fts5(title,buyer,description,cpv,ref,process,supplier, content='notices',content_rowid='id',tokenize='unicode61 remove_diacritics 2');
        CREATE TRIGGER IF NOT EXISTS words_ai AFTER INSERT ON notices BEGIN INSERT INTO words(rowid,title,buyer,description,cpv,ref,process,supplier) VALUES(new.id,new.title,new.buyer,new.description,new.cpv,new.ref,new.process,new.supplier); END;
        CREATE TRIGGER IF NOT EXISTS words_au AFTER UPDATE ON notices BEGIN INSERT INTO words(words,rowid,title,buyer,description,cpv,ref,process,supplier) VALUES('delete',old.id,old.title,old.buyer,old.description,old.cpv,old.ref,old.process,old.supplier); INSERT INTO words(rowid,title,buyer,description,cpv,ref,process,supplier) VALUES(new.id,new.title,new.buyer,new.description,new.cpv,new.ref,new.process,new.supplier); END;
        CREATE TABLE IF NOT EXISTS source_documents(document_key TEXT PRIMARY KEY,document_json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS source_locations(source TEXT NOT NULL,ref TEXT NOT NULL,document_key TEXT NOT NULL,locator_json TEXT NOT NULL,PRIMARY KEY(source,ref));
        CREATE TABLE IF NOT EXISTS atlas_metadata(name TEXT PRIMARY KEY,value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS runs(id INTEGER PRIMARY KEY,created TEXT,start TEXT,end TEXT,status TEXT);
        CREATE TABLE IF NOT EXISTS tasks(run INTEGER,source TEXT,cursor TEXT,status TEXT DEFAULT 'Queued',processed INTEGER DEFAULT 0,message TEXT DEFAULT '',PRIMARY KEY(run,source));
        """)
        c.execute("update runs set status='Paused' where status in ('Collecting','Pausing')")
        c.execute("update tasks set status='Paused' where status='Collecting'")
        if not c.execute("SELECT 1 FROM atlas_metadata WHERE name='iso_dates_v1'").fetchone():
            rows = c.execute("""SELECT id,published,deadline FROM notices
                WHERE (published != '' AND date(published,'+0 days') IS NOT published)
                   OR (deadline != '' AND date(deadline,'+0 days') IS NOT deadline)""").fetchall()
            c.executemany("UPDATE notices SET published=?,deadline=? WHERE id=?",
                          [(clean_date(r["published"]), clean_date(r["deadline"]), r["id"]) for r in rows])
            c.execute("INSERT INTO atlas_metadata VALUES('iso_dates_v1','1')")
    access.configure(DB)

def store_locations(c, source, rows):
    """Share one request description across all records of a response."""
    documents = {}
    locations = []
    for r in rows:
        doc = r.get("document")
        if doc is None:
            continue
        identity = id(doc)
        if identity not in documents:
            serial = json.dumps(doc, ensure_ascii=False, sort_keys=True, separators=(",",":"))
            documents[identity] = (hashlib.sha256(serial.encode()).hexdigest(), serial)
        key = documents[identity][0]
        locations.append((source, r["ref"], key, json.dumps(r["locator"], ensure_ascii=False)))
    c.executemany("INSERT OR IGNORE INTO source_documents VALUES(?,?)", documents.values())
    c.executemany("INSERT INTO source_locations VALUES(?,?,?,?) ON CONFLICT(source,ref) DO UPDATE SET document_key=excluded.document_key,locator_json=excluded.locator_json", locations)

def needs_seed_provenance():
    with conn() as c:
        return not c.execute("SELECT 1 FROM atlas_metadata WHERE name='provenance_seed_v1'").fetchone()

def import_seed_provenance(path):
    """Enrich older libraries only when both the stored origin and payload agree."""
    with conn() as c:
        c.execute("ATTACH DATABASE ? AS seed", (str(path),))
        if c.execute("SELECT 1 FROM seed.sqlite_master WHERE name='source_locations'").fetchone():
            c.execute("INSERT OR IGNORE INTO source_documents SELECT * FROM seed.source_documents")
            c.execute("""INSERT OR IGNORE INTO source_locations
                      SELECT l.source,l.ref,l.document_key,l.locator_json FROM seed.source_locations AS l
                      JOIN seed.notices AS s USING(source,ref) JOIN main.notices AS n USING(source,ref)
                      WHERE s.raw=n.raw AND s.origin=n.origin""")
            c.execute("INSERT OR REPLACE INTO atlas_metadata VALUES('provenance_seed_v1','1')")

def store(source,rows):
    if not rows:
        return
    stamp=now()
    values=[]
    for r in rows:
        raw=zlib.compress(json.dumps(r["raw"],ensure_ascii=False,separators=(",",":")).encode(),3)
        values.append([source]+[clean_date(r[k]) if k in {"published", "deadline"} else r[k] for k in FIELDS]+[raw,stamp])
    updates=", ".join(f"{k}=excluded.{k}" for k in FIELDS if k!="ref")
    with conn() as c:
        c.executemany(f"INSERT INTO notices(source,{COLUMNS},raw,retrieved) VALUES({','.join('?' for _ in values[0])}) ON CONFLICT(source,ref) DO UPDATE SET {updates},raw=excluded.raw,retrieved=excluded.retrieved WHERE notices.raw != excluded.raw OR notices.origin != excluded.origin", values)
        store_locations(c, source, rows)

def filters(q):
    conditions,args=[],[]
    terms=re.findall(r"\w+",q.get("q","")[:400],re.UNICODE)
    if terms:
        conditions.append("id IN (SELECT rowid FROM words WHERE words MATCH ?)")
        args.append(" AND ".join('"'+w+'"*' for w in terms))
    for col in ("country","stage","source"):
        if q.get(col):
            conditions.append(col+"=?")
            args.append(q[col])
    for key,op in (("from",">="),("to","<=")):
        if q.get(key):
            conditions.append("published"+op+"?")
            args.append(date.fromisoformat(q[key]).isoformat())
    if q.get("upcoming")=="1":
        conditions.append("stage='Tender' AND deadline>=?")
        args.append(date.today().isoformat())
    return (" WHERE "+" AND ".join(conditions) if conditions else ""),args

def search(q):
    where,args=filters(q)
    limit=25
    page=max(1,int(q.get("page","1")))
    order=ORDER.get(q.get("sort"),ORDER["recent"])
    with conn() as c:
        total=c.execute("SELECT COUNT(*) FROM notices"+where,args).fetchone()[0]
        page=min(page,max(1,(total+limit-1)//limit))
        rows=c.execute(f"SELECT {LIST_COLUMNS} FROM notices{where} ORDER BY {order} LIMIT ? OFFSET ?",args+[limit,(page-1)*limit])
        return dict(total=total,page=page,limit=limit,rows=[dict(r) for r in rows])

def stats():
    with conn() as c:
        by_source={r[0]:dict(count=r[1],first=r[2],last=r[3]) for r in c.execute("SELECT source,count(*),min(published),max(published) FROM notices GROUP BY source")}
        countries=[r[0] for r in c.execute("SELECT DISTINCT country FROM notices ORDER BY country")]
        return dict(total=sum(v["count"] for v in by_source.values()),countries=countries,
            stages=[r[0] for r in c.execute("SELECT DISTINCT stage FROM notices ORDER BY stage")],
            sources={k:dict(short=v[0],name=v[1],region=v[2],description=v[3],url=v[4],excluded=access.excluded("source:"+k),**by_source.get(k,dict(count=0,first=None,last=None))) for k,v in SOURCES.items()} | ({"portal":dict(short="Portal preview",name="Discovered portals",region="User-added",description="Checked notice samples added from public portals.",url="",**by_source["portal"])} if "portal" in by_source else {}),
            latest=max((v["last"] for v in by_source.values()),default=None), today=date.today().isoformat())

def detail(key,raw=False):
    with conn() as c:
        if raw:
            r=c.execute("SELECT raw FROM notices WHERE id=?",(key,)).fetchone()
        else:
            cols = "id,source,"+COLUMNS+",retrieved,"+TRACE_COLUMNS
            r=c.execute("SELECT "+cols+" FROM notices"+TRACE_JOIN+"WHERE id=?",(key,)).fetchone()
        if not r:
            raise KeyError("Record not found")
        return json.loads(zlib.decompress(r["raw"])) if raw else with_provenance(r)

def with_provenance(row):
    record = dict(row)
    record["provenance"] = provenance.for_record(record)
    del record["document_json"], record["locator_json"]
    return record

def status():
    with conn() as c:
        run=c.execute("SELECT * FROM runs ORDER BY id DESC LIMIT 1").fetchone()
        if not run:
            return None
        return {**dict(run),"tasks":[dict(r) for r in c.execute("SELECT * FROM tasks WHERE run=? ORDER BY source",(run["id"],))]}

def start_job(start,end,sources):
    a,b=date.fromisoformat(start),date.fromisoformat(end)
    if a>b or b>date.today() or not sources or any(s not in SOURCES for s in sources):
        raise ValueError("Choose at least one source and a valid date range ending today or earlier.")
    sources = [source for source in sources if not access.excluded("source:" + source)]
    if not sources:
        raise ValueError("The selected publishers are excluded because they require a CAPTCHA.")
    with GUARD:
        current=status()
        if current and current["status"] in ("Collecting","Pausing"):
            raise ValueError("A collection is already running.")
        with conn() as c:
            run=c.execute("INSERT INTO runs(created,start,end,status) VALUES(?,?,?,'Collecting')",(now(),start,end)).lastrowid
            c.executemany("INSERT INTO tasks(run,source,cursor) VALUES(?,?,?)",[(run,s,start) for s in dict.fromkeys(sources)])
        dispatch(run)
    return status()

def resume():
    with GUARD:
        run=status()
        if not run or run["status"] not in ("Paused","Needs attention"):
            raise ValueError("There is no interrupted collection to resume.")
        with conn() as c:
            c.execute("UPDATE runs SET status='Collecting' WHERE id=?",(run["id"],))
            c.execute("UPDATE tasks SET status='Queued',message='' WHERE run=? AND status NOT IN ('Complete','Excluded')",(run["id"],))
        dispatch(run["id"])
    return status()

def pause():
    with GUARD:
        with conn() as c:
            c.execute("UPDATE runs SET status='Pausing' WHERE status='Collecting'")
        STOP.set()
    return status()

def unit_end(source,cursor,last):
    if source=="ie":
        return last
    if source=="nl":
        return min(last,date(cursor.year,12,31))
    if source=="es":
        month_end=(cursor.replace(day=28)+timedelta(days=4)).replace(day=1)-DAY
        return min(last,month_end)
    return cursor

def collect_source(run_id,source,cursor,last):
    try:
        if source=="de":
            cursor=max(cursor,date(2022,12,1))
        while cursor<=last and not STOP.is_set():
            end=unit_end(source,cursor,last)
            stamp=cursor.isoformat()
            with conn() as c:
                c.execute("UPDATE tasks SET status='Collecting',message=? WHERE run=? AND source=?",(stamp,run_id,source))
            # The cursor is committed only after the whole publisher window succeeds.
            # Replaying a partial page/day is safe because publisher records have stable keys.
            with access.owner("source:" + source):
                for batch in COLLECTORS[source](stamp,end.isoformat(),CACHE):
                    if STOP.is_set():
                        return
                    store(source,batch)
            cursor=end+DAY
            with conn() as c:
                c.execute("UPDATE tasks SET cursor=?,processed=processed+1 WHERE run=? AND source=?",(cursor.isoformat(),run_id,source))
        if not STOP.is_set():
            with conn() as c:
                c.execute("UPDATE tasks SET status='Complete', message='' WHERE run=? AND source=?",(run_id,source))
    except access.CaptchaExcluded as exc:
        with conn() as c:
            c.execute("UPDATE tasks SET status='Excluded',message=? WHERE run=? AND source=?", (str(exc),run_id,source))
    except Exception as e:
        msg=str(e) or type(e).__name__
        if hasattr(e,"code"):
            msg=f"Publisher returned HTTP {e.code} for {cursor.isoformat()}."
        with conn() as c:
            c.execute("UPDATE tasks SET status='Error',message=? WHERE run=? AND source=?",(msg[:400],run_id,source))

def dispatch(run_id):
    from concurrent.futures import ThreadPoolExecutor
    STOP.clear()
    def worker():
        with conn() as c:
            last=date.fromisoformat(c.execute("SELECT end FROM runs WHERE id=?",(run_id,)).fetchone()[0])
            tasks=list(c.execute("SELECT source,cursor FROM tasks WHERE run=? AND status NOT IN ('Complete','Excluded') ORDER BY CASE WHEN source IN ('ie','nl') THEN 1 ELSE 0 END",(run_id,)))
        with ThreadPoolExecutor(max_workers=3) as pool:
            futures=[pool.submit(collect_source,run_id,t["source"],date.fromisoformat(t["cursor"]),last) for t in tasks]
            for f in futures:
                f.result()
        with GUARD,conn() as c:
            c.execute("UPDATE tasks SET status='Paused' WHERE run=? AND status IN ('Queued','Collecting')",(run_id,))
            states={r[0] for r in c.execute("SELECT status FROM tasks WHERE run=?",(run_id,))}
            result="Complete" if states <= {"Complete","Excluded"} else "Paused" if STOP.is_set() else "Needs attention"
            c.execute("UPDATE runs SET status=? WHERE id=?",(result,run_id))
    threading.Thread(target=worker,daemon=True,name="collector").start()
