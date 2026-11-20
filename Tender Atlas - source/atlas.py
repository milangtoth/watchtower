"""Tender Atlas runs entirely on the local computer."""
import argparse
import csv
import gzip
import io
import json
import mimetypes
import os
import shutil
import sys
import threading
import tempfile
import webbrowser
import zlib
from datetime import date
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit
from urllib.request import urlopen
from discovery import service as portals
import watchtower_client
import indexdb as db
from feeds import ROOT

class App(BaseHTTPRequestHandler):
    server_version="TenderAtlas/1.0"
    def log_message(self, *args):
        pass
    def valid_host(self):
        return self.headers.get("Host","") in (f"127.0.0.1:{self.server.server_port}",f"localhost:{self.server.server_port}")
    def headers_for(self,kind,code=200,download=None):
        self.send_response(code)
        self.send_header("Content-Type",kind)
        self.send_header("Cache-Control","no-cache")
        self.send_header("X-Content-Type-Options","nosniff")
        self.send_header("Referrer-Policy","no-referrer")
        self.send_header("Content-Security-Policy","default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'")
        if download:
            self.send_header("Content-Disposition",'attachment; filename="'+download+'"')
        self.end_headers()
    def answer(self,obj,code=200):
        self.headers_for("application/json; charset=utf-8",code)
        self.wfile.write(json.dumps(obj,ensure_ascii=False).encode())
    def do_GET(self):
        if not self.valid_host():
            return self.send_error(403)
        parsed=urlsplit(self.path)
        path=parsed.path
        q={k:v[-1] for k,v in parse_qs(parsed.query).items()}
        try:
            if watchtower_client.handles("GET", path):
                return watchtower_client.forward(self)
            if path=="/api/identity":
                return self.answer({"app":"Tender Atlas","version":2})
            if path=="/api/portals":
                return self.answer(portals.saved())
            if path=="/api/portal-examples":
                self.headers_for("application/x-ndjson", download="tender-atlas-portal-examples.jsonl")
                for row in portals.examples():
                    self.wfile.write((json.dumps(row,ensure_ascii=False)+"\n").encode())
                return
            if path=="/api/stats":
                return self.answer(db.stats())
            if path=="/api/notices":
                return self.answer(db.search(q))
            if path=="/api/status":
                return self.answer(db.status())
            if path.startswith("/api/notice/"):
                key=int(path.split("/")[3])
                return self.answer(db.detail(key, path.endswith("/raw")))
            if path=="/api/export":
                return self.export(q)
            files={"/":"index.html","/app.js":"app.js","/portals.js":"portals.js","/managed.js":"managed.js","/app.css":"app.css","/atlas-map.png":"atlas-map.png","/icon.png":"icon.png"}
            if path not in files:
                return self.send_error(404)
            file=ROOT/"static"/files[path]
            data=file.read_bytes()
            self.headers_for(mimetypes.guess_type(file.name)[0] or "application/octet-stream")
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass
        except (ValueError, KeyError, IndexError) as e:
            self.answer({"error":str(e)},400)
        except Exception as e:
            self.answer({"error":str(e)},500)
    def do_DELETE(self):
        if not self.valid_host() or self.headers.get("Origin") not in (None,"http://"+self.headers.get("Host","")):
            return self.send_error(403)
        if watchtower_client.handles("DELETE", urlsplit(self.path).path):
            return watchtower_client.forward(self)
        return self.send_error(404)
    def do_POST(self):
        if not self.valid_host() or self.headers.get("Origin") not in (None,"http://"+self.headers.get("Host","")) or self.headers.get("X-Atlas-Request")!="1":
            return self.send_error(403)
        try:
            size=int(self.headers.get("Content-Length","0"))
            if size>10000:
                return self.send_error(413)
            body=json.loads(self.rfile.read(size) or b"{}")
            if watchtower_client.handles("POST", self.path):
                return watchtower_client.forward(self, json.dumps(body).encode())
            if self.path=="/api/portal-preview":
                return self.answer(portals.preview(body["url"],refresh=body.get("refresh") is True))
            if self.path=="/api/portal-label":
                return self.answer(portals.label(body["entry"],body["url"],body["label"]))
            if self.path=="/api/portal-import":
                return self.answer(portals.add_to_register(body["entry"]))
            if self.path=="/api/collect":
                return self.answer(db.start_job(body["start"],body["end"],body["sources"]))
            if self.path=="/api/pause":
                return self.answer(db.pause())
            if self.path=="/api/resume":
                return self.answer(db.resume())
            if self.path=="/api/quit":
                db.pause()
                self.answer({"closing":True})
                threading.Thread(target=self.server.shutdown,daemon=True).start()
                return
            self.send_error(404)
        except (ValueError,KeyError,TypeError) as e:
            self.answer({"error":str(e)},400)
    def export(self,q):
        raw=q.get("format")=="jsonl"
        where,args=db.filters(q)
        columns = ["source", *db.FIELDS, "retrieved"]
        cols = ",".join(columns)
        order=db.ORDER.get(q.get("sort"),db.ORDER["recent"])
        with db.conn() as c:
            cursor=c.execute("SELECT "+cols+","+db.TRACE_COLUMNS+(",raw" if raw else "")+" FROM notices"+db.TRACE_JOIN+where+" ORDER BY "+order,args)
            fname="tender-atlas-"+date.today().isoformat()+(".jsonl" if raw else ".csv")
            self.headers_for("application/x-ndjson" if raw else "text/csv; charset=utf-8",download=fname)
            if raw:
                for r in cursor:
                    record=db.with_provenance(r)
                    record["raw"]=json.loads(zlib.decompress(record["raw"]))
                    self.wfile.write((json.dumps(record,ensure_ascii=False)+"\n").encode())
            else:
                buffer=io.StringIO()
                writer=csv.writer(buffer)
                self.wfile.write(b"\xef\xbb\xbf")
                writer.writerow(columns + ["provenance"])
                for r in cursor:
                    record = db.with_provenance(r)
                    cells = [record[k] for k in columns]
                    cells.append(json.dumps(record["provenance"], ensure_ascii=False, separators=(",",":")))
                    writer.writerow(["'"+v if isinstance(v,str) and v.lstrip().startswith(("=","+","-","@")) else v for v in cells])
                    if buffer.tell()>65536:
                        self.wfile.write(buffer.getvalue().encode())
                        buffer.seek(0); buffer.truncate(0)
                self.wfile.write(buffer.getvalue().encode())

def run():
    parser=argparse.ArgumentParser(description="Tender Atlas local procurement explorer")
    parser.add_argument("--data",type=Path)
    parser.add_argument("--port",type=int,default=int(os.environ.get("PORT","43871")))
    parser.add_argument("--no-browser",action="store_true")
    args=parser.parse_args()
    host=os.environ.get("HOST","127.0.0.1")
    url=f"http://{host}:{args.port}/"
    try:
        server=ThreadingHTTPServer((host,args.port),App)
    except OSError:
        with urlopen(url+"api/identity",timeout=3) as response:
            if json.load(response).get("app")!="Tender Atlas":
                raise RuntimeError("The local port is in use by another app.")
        if not args.no_browser:
            webbrowser.open(url)
        return
    here=Path(sys.executable).parent if getattr(sys,"frozen",False) else ROOT
    data=args.data or here/"Atlas library"
    data.mkdir(parents=True,exist_ok=True)
    path=data/"atlas.sqlite"
    seed=ROOT/"starter.sqlite.gz"
    if not seed.is_file():
        seed=here/"starter.sqlite.gz"
    if not path.exists() and seed.is_file():
        tmp=path.with_suffix(".installing")
        with gzip.open(seed,"rb") as src,tmp.open("wb") as out:
            shutil.copyfileobj(src,out,1024*1024)
        tmp.replace(path)
    db.setup(path)
    portals.setup()
    if seed.is_file() and db.needs_seed_provenance():
        with tempfile.TemporaryDirectory(dir=data) as scratch:
            reference=Path(scratch)/"published.sqlite"
            with gzip.open(seed,"rb") as src, reference.open("wb") as out:
                shutil.copyfileobj(src,out,1024*1024)
            db.import_seed_provenance(reference)
    if not args.no_browser:
        webbrowser.open(url)
    try:
        server.serve_forever()
    finally:
        server.server_close()

if __name__=="__main__":
    try:
        run()
    except Exception as exc:
        if getattr(sys,"frozen",False):
            import ctypes
            ctypes.windll.user32.MessageBoxW(None,str(exc),"Tender Atlas",16)
        else:
            raise
