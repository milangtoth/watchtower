"""Exercise the Atlas HTTP boundary with a real worker and deterministic publisher."""
import socket
import sys
import threading
import time
from pathlib import Path
from types import SimpleNamespace
from http.server import ThreadingHTTPServer

import httpx
import uvicorn

from app import config
from app.fetch.http import Reader
from app.main import app


def test_atlas_collects_and_serves_documents(tmp_path, monkeypatch):
    monkeypatch.syspath_prepend(str(Path(__file__).resolve().parents[1] / "Tender Atlas - source"))
    from atlas import App

    async def publisher(self, url, **kwargs):
        if url.endswith('/notice'):
            body, mime = b'<h1>Road tender</h1><p>Submission deadline</p>', 'text/html'
        elif url.endswith('/spec.pdf'):
            body, mime = b'%PDF-specification', 'application/pdf'
        else:
            body = b"""<rss><channel><item><guid>R-42</guid><title>Road tender</title>
            <link>https://example.org/notice</link><enclosure url="/spec.pdf"/>
            </item></channel></rss>"""
            mime = 'application/rss+xml'
        return SimpleNamespace(url=url, body=body, mime=mime)

    monkeypatch.setattr(Reader, 'get', publisher)
    monkeypatch.setattr(config, 'DB_PATH', tmp_path / 'watchtower.db')
    monkeypatch.setattr(config, 'FILES_DIR', tmp_path / 'files')
    monkeypatch.setattr(config, 'OPENAI_API_KEY', '')
    with socket.socket() as sock, ThreadingHTTPServer(('127.0.0.1', 0), App) as atlas:
        sock.bind(('127.0.0.1', 0))
        worker = uvicorn.Server(uvicorn.Config(app, log_level='error'))
        worker_thread = threading.Thread(target=worker.run, kwargs={'sockets': [sock]})
        worker_thread.start()
        monkeypatch.setenv('WATCHTOWER_URL', f'http://127.0.0.1:{sock.getsockname()[1]}')
        atlas_thread = threading.Thread(target=atlas.serve_forever)
        atlas_thread.start()
        try:
            with httpx.Client(base_url=f'http://127.0.0.1:{atlas.server_port}',
                              headers={'X-Atlas-Request': '1'}, trust_env=False) as client:
                for _ in range(100):
                    if worker.started:
                        break
                    time.sleep(.02)
                assert 'portal-add-form' in client.get('/').text
                registration = client.post('/portals', json={'url': 'https://example.org/feed'})
                assert registration.status_code == 201
                path = '/portals/' + registration.json()['id']
                for _ in range(100):
                    if client.get(path).json()['state'] == 'serving':
                        break
                    time.sleep(.02)
                tender, = client.get(path + '/tenders').json()
                assert tender['id'] == 'R-42'
                document, = tender['documents']
                assert client.get(document['path']).content == b'%PDF-specification'
                assert client.get(path + '/runs').json()[0]['result'] == 'ok'
                assert client.post(path + '/refresh', json={}).status_code == 202
                assert client.delete(path).status_code == 204
                assert client.get(path).status_code == 404
        finally:
            atlas.shutdown()
            worker.should_exit = True
            worker_thread.join(5)
            atlas_thread.join(5)
