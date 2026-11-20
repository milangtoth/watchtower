"""Regression tests for source locations, historical data and matching exports."""
import csv
import hashlib
import io
import json
import tempfile
import threading
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch
from urllib.request import urlopen

import atlas
import feeds
import indexdb as db
import provenance as prov


class Response(io.BytesIO):
    def geturl(self):
        return "https://publisher.example/resolved"


class ProvenanceTests(unittest.TestCase):
    def test_post_fingerprint_and_request_snapshot(self):
        payload = {"query": "publication-date = 20261119", "limit": 2}
        body = b'{"notices":[]}'
        with patch.object(feeds, "get", return_value=Response(body)):
            _, doc = feeds.resource(feeds.TED, payload)
        payload["iterationNextToken"] = "another-page"
        self.assertNotIn("iterationNextToken", doc["request"]["json_body"])
        self.assertEqual(doc["request"]["method"], "POST")
        self.assertEqual(doc["response_url"], "https://publisher.example/resolved")
        self.assertEqual(doc["response_sha256"], hashlib.sha256(body).hexdigest())

    def test_csv_position_counts_data_records(self):
        data = b'releases/0/id,releases/0/date,releases/0/tender/title\r\n1,2026-01-01,"a\r\nb"\r\n2,2026-01-01,c\r\n'
        with patch.object(feeds, "get", return_value=Response(data)):
            rows = next(feeds.cf("2026-01-01", "2026-01-01", None))
        self.assertEqual(rows[1]["locator"], prov.csv_location(2))

    def test_zip_member_and_json_pointer(self):
        stream = io.BytesIO()
        entry = {"id":"abc", "date":"2025-01-01", "tender":{"title":"Works"}}
        with zipfile.ZipFile(stream, "w") as z:
            z.writestr("folder with space/abc-1.json", json.dumps({"releases":[entry]}))
        with patch.object(feeds, "resource", return_value=(stream.getvalue(), {})):
            row = next(feeds.germany("2025-01-01", "2025-01-01", None))[0]
        self.assertEqual(row["locator"], prov.json_location("/releases/0", "folder with space/abc-1.json"))

    def test_atom_filter_preserves_original_position(self):
        xml = '<feed xmlns="http://www.w3.org/2005/Atom"><entry><updated>2025-01-01</updated></entry><entry><updated>2025-01-02</updated><id>https://host/2</id><title>B</title><link href="https://host/item"/><ContractFolderStatus><ContractFolderID>p</ContractFolderID></ContractFolderStatus></entry></feed>'
        rows, _ = feeds.es_page(feeds.ET.fromstring(xml),"2025-01-02","2025-01-02","https://host/file.zip",{},"one.atom")
        self.assertEqual(rows[0]["locator"], prov.atom_location(2,"one.atom"))

    def test_provenance_upgrade_and_exports(self):
        with tempfile.TemporaryDirectory() as tmp:
            db.setup(Path(tmp)/"library.sqlite")
            origin = "https://publisher.example/notices?country=NL&tag=a%2Fb"
            item = feeds.row("a","Works","Council","Netherlands","Tender","2025-01-01","https://publisher.example/item/a",{"id":"a"},origin)
            db.store("nl",[item])
            self.assertEqual(db.detail(1)["provenance"]["evidence"],"legacy_url_only")
            self.assertIsNone(db.detail(1)["provenance"]["record_locator"])
            doc = prov.archive_evidence(origin,"a"*64)
            db.store("nl",[prov.located(item,doc,prov.json_location("/releases/7"))])
            self.assertEqual(db.stats()["total"],1)
            provenance = db.detail(1)["provenance"]
            self.assertEqual(provenance["request"]["path"],"/notices")
            self.assertEqual(provenance["request"]["query"],"country=NL&tag=a%2Fb")
            self.assertEqual(db.detail(1,True),{"id":"a"})
            server=atlas.ThreadingHTTPServer(("127.0.0.1",0),atlas.App)
            thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
            try:
                base=f"http://127.0.0.1:{server.server_port}/api/export?source=nl"
                with urlopen(base+"&format=jsonl") as resp:
                    self.assertEqual(json.loads(resp.read())["provenance"],provenance)
                with urlopen(base) as resp:
                    row=next(csv.DictReader(io.StringIO(resp.read().decode("utf-8-sig"))))
                    self.assertEqual(json.loads(row["provenance"]),provenance)
            finally:
                server.shutdown();server.server_close();thread.join()


if __name__ == "__main__":
    unittest.main()
