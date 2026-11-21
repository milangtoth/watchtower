"""Offline tests for the Romanian, Swiss, Greek, Norwegian and Latvian collectors."""
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import feeds
import provenance as prov

DOC = {"evidence": "captured"}


def pages(*responses):
    """json_resource stand-in that returns each response in turn and records the calls."""
    calls = []
    def fake(url, body=None, discovered_from=None, headers=None):
        calls.append((url, body))
        return responses[len(calls) - 1], DOC
    return fake, calls


def collect(gen):
    return [r for batch in gen for r in batch]


class RomaniaTests(unittest.TestCase):
    def test_routes_follow_each_notice_type(self):
        c = {"cNoticeId": 7, "sysNoticeVersionId": 2}
        self.assertEqual(feeds.ro_url("c", {**c, "noticeNo": "SCN1"}), "https://www.e-licitatie.ro/pub/notices/simplified-notice/v2/view/7")
        self.assertEqual(feeds.ro_url("c", {**c, "noticeNo": "CN1"}), "https://www.e-licitatie.ro/pub/notices/c-notice/v2/view/7")
        self.assertTrue(feeds.ro_url("c", {**c, "noticeNo": "CN1", "sysNoticeVersionId": 1}).endswith("/c-notice/v1/view/7"))
        award = lambda typ: feeds.ro_url("ca", {"caNoticeId": 9, "sysNoticeTypeId": typ, "sysNoticeVersionId": 2})
        self.assertTrue(award(3).endswith("/ca-notices/view-c/9"))
        self.assertTrue(award(13).endswith("/ca-notices/view-rfq/9"))
        self.assertTrue(award(20).endswith("/ca-notices/view-rfq/9"))
        self.assertTrue(award(16).endswith("/ca-notices-pc/view/9"))
        self.assertTrue(award(18).endswith("/ca-notices/view-snc/9"))

    def test_both_lists_page_until_total(self):
        item = lambda n: {"cNoticeId": n, "noticeNo": f"CN{n}", "contractTitle": "Drum", "contractingAuthorityNameAndFN": "123 - Primaria X",
                          "cpvCodeAndName": "45233140-2 - Lucrari de drumuri", "estimatedValueRon": 10.5, "sysNoticeState": {"text": "Publicat"}}
        award = {"caNoticeId": 5, "noticeNo": "CAN5", "contractTitle": "Pod", "sysNoticeTypeId": 3, "ronContractValue": 0,
                 "sysNoticeState": {"text": "Anulat"}}
        fake, calls = pages({"total": 101, "items": [item(i) for i in range(100)]}, {"total": 101, "items": [item(100)]},
                            {"total": 1, "items": [award]})
        with patch.object(feeds, "json_resource", fake):
            rows = collect(feeds.romania("2026-11-19", "2026-11-19", None))
        self.assertEqual(len(calls), 3)
        self.assertEqual([c[1]["pageIndex"] for c in calls], [0, 1, 0])
        self.assertEqual(calls[0][1]["startPublicationDate"], "2026-11-19T00:00:00.000Z")
        first, last = rows[0], rows[-1]
        self.assertEqual((first["buyer"], first["cpv"], first["amount"], first["stage"]), ("Primaria X", "45233140", "10.5", "Tender"))
        self.assertEqual((last["stage"], last["amount"], last["value_type"]), ("Cancellation", "", "Award value"))
        self.assertEqual(rows[100]["locator"], prov.json_location("/items/0"))


class SwitzerlandTests(unittest.TestCase):
    def project(self, n, pub="tender"):
        return {"id": f"id-{n}", "publicationId": f"p{n}", "publicationNumber": f"{n}-01", "projectNumber": str(n), "pubType": pub,
                "title": {"de": None, "fr": "Travaux"}, "procOfficeName": {"de": "Gemeinde"}, "publicationDate": "2026-11-19"}

    def test_cursor_pagination_and_labels(self):
        fake, calls = pages({"projects": [self.project(i) for i in range(20)], "pagination": {"lastItem": "c1", "itemsPerPage": 20}},
                            {"projects": [self.project(99, "award")], "pagination": {"lastItem": "c2", "itemsPerPage": 20}})
        with patch.object(feeds, "json_resource", fake):
            rows = collect(feeds.switzerland("2026-11-19", "2026-11-19", None))
        self.assertIn("lastItem=c1", calls[1][0])
        self.assertEqual(len(rows), 21)
        self.assertEqual((rows[0]["title"], rows[0]["buyer"]), ("Travaux", "Gemeinde"))
        self.assertEqual(rows[-1]["stage"], "Award")
        self.assertEqual(rows[-1]["url"], "https://www.simap.ch/en/project-detail/id-99")

    def test_repeated_cursor_is_an_error(self):
        full = {"projects": [self.project(i) for i in range(20)], "pagination": {"lastItem": "same", "itemsPerPage": 20}}
        fake, _ = pages(full, full)
        with patch.object(feeds, "json_resource", fake), self.assertRaises(ValueError):
            collect(feeds.switzerland("2026-11-19", "2026-11-19", None))


class GreeceTests(unittest.TestCase):
    def test_labels_cpv_and_pages(self):
        notice = {"referenceNumber": "26PROC000000001", "title": "Προμήθεια", "submissionDate": "2026-11-19T10:00:00",
                  "organization": {"key": "6254", "value": "ΔΗΜΟΣ ΠΡΕΣΠΩΝ"}, "noticeType": {"key": "4", "value": "Πρόσκληση"},
                  "finalSubmissionDate": "2026-11-26T15:00:00", "totalCostWithoutVAT": 100.0, "cancelled": False,
                  "objectDetails": [{"cpvs": [{"key": "45000000-7"}, {"key": "44111200-3"}]}, {"cpvs": [{"key": "45000000-7"}]}]}
        fake, calls = pages({"content": [notice], "last": False}, {"content": [{**notice, "referenceNumber": "26PROC000000002", "cancelled": True}], "last": True})
        with patch.object(feeds, "json_resource", fake), patch.object(feeds.time, "sleep"):
            rows = collect(feeds.greece("2026-11-19", "2026-11-19", None))
        self.assertEqual([c[0].rsplit("=", 1)[1] for c in calls], ["0", "1"])
        self.assertEqual(calls[0][1], {"dateFrom": "2026-11-19", "dateTo": "2026-11-19"})
        self.assertEqual(rows[0]["buyer"], "ΔΗΜΟΣ ΠΡΕΣΠΩΝ")
        self.assertEqual(rows[0]["cpv"], "45000000 · 44111200")
        self.assertTrue(rows[0]["description"].startswith("Πρόσκληση"))
        self.assertEqual(rows[0]["url"], feeds.GR + "/attachment/26PROC000000001")
        self.assertEqual(rows[1]["stage"], "Cancellation")


class NorwayTests(unittest.TestCase):
    def test_stage_mapping(self):
        hits = [{"id": "2026-1", "heading": "Kraft", "buyer": [{"name": "Askøy kommune"}], "type": "ANNOUNCEMENT_OF_COMPETITION",
                 "publicationDate": "2026-11-19", "estimatedValue": {"amount": 5.0, "currencyCode": "NOK"}},
                {"id": "2026-2", "heading": "Avlyst", "type": "ANNOUNCEMENT_OF_COMPETITION", "status": "CANCELLED", "publicationDate": "2026-11-19"},
                {"id": "2026-3", "heading": "Kontrakt", "type": "ANNOUNCEMENT_OF_CONCLUSION_OF_CONTRACT", "publicationDate": "2026-11-19"}]
        fake, calls = pages({"numHitsTotal": 3, "numHitsAccessible": 1000, "hits": hits})
        with patch.object(feeds, "json_resource", fake):
            rows = collect(feeds.norway("2026-11-19", "2026-11-19", None))
        self.assertEqual(calls[0][1]["facets"], {"publicationDate": {"from": "2026-11-19", "to": "2026-11-19"}})
        self.assertEqual([r["stage"] for r in rows], ["Tender", "Cancellation", "Award"])
        self.assertEqual((rows[0]["buyer"], rows[0]["amount"], rows[0]["currency"]), ("Askøy kommune", "5.0", "NOK"))
        self.assertEqual(rows[0]["url"], "https://doffin.no/notices/2026-1")

    def test_refuses_truncated_results(self):
        fake, _ = pages({"numHitsTotal": 1500, "numHitsAccessible": 1000, "hits": []})
        with patch.object(feeds, "json_resource", fake), self.assertRaises(ValueError):
            collect(feeds.norway("2026-01-01", "2026-03-01", None))


class LatviaTests(unittest.TestCase):
    HEADER = ("Iepirkuma_ID,Iepirkuma_nosaukums,Iepirkuma_identifikacijas_numurs,Pasutitaja_nosaukums,CPV_kods_galvenais_prieksmets,"
              "CPV_kodi_papildus_prieksmeti,Iepirkuma_statuss,Iepirkuma_izsludinasanas_datums,Piedavajumu_iesniegsanas_datums,"
              "Planota_ligumcena,Ligumcenas_valuta,Hipersaite_EIS_kura_pieejams_zinojums,Iepirkuma_dalas_nr,Iepirkuma_dalas_nosaukums,"
              "Iepirkuma_dalas_statuss,Dalas_planota_ligumcena,Dalas_ligumcenas_valuta\n")

    def test_filters_day_keys_lots_and_maps_status(self):
        rows_csv = (self.HEADER
                    + '1,Ceļš,A-1,Dome,45233140-2 Ceļu darbi,,Izsludināts,18.11.2026,01.12.2026,"1000,50",EUR,https://www.eis.gov.lv/EKEIS/Supplier/Procurement/1,,,,,\n'
                    + '2,Skola,B-2,Novads,45214200-2 Skolas,71000000-8 Projektēšana;71300000-1 Inženieri,Pārtraukts,19.11.2026,,,,https://www.eis.gov.lv/EKEIS/Supplier/Procurement/2,1,Ēka,Līgums piešķirts,500,EUR\n'
                    + '2,Skola,B-2,Novads,45214200-2 Skolas,,Pārtraukts,19.11.2026,,,,https://www.eis.gov.lv/EKEIS/Supplier/Procurement/2,2,Sēta,,,\n')
        meta = {"result": {"resources": [{"name": "EIS_E_IEPIRKUMI_IZSLUDINATIE_2026.csv",
                                          "url": "https://data.gov.lv/dati/dataset/x/resource/y/download/file.csv"}]}}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "lv.csv"
            path.write_text(rows_csv, encoding="utf-8-sig")
            with patch.object(feeds, "js", return_value=meta), patch.object(feeds, "cached", return_value=(path, DOC)):
                rows = collect(feeds.latvia("2026-11-19", "2026-11-19", None))
        self.assertEqual([r["ref"] for r in rows], ["2/1", "2/2"])
        self.assertEqual(rows[0]["title"], "Skola — Ēka")
        self.assertEqual(rows[0]["stage"], "Award")          # lot status wins over the procurement status
        self.assertEqual(rows[1]["stage"], "Cancellation")   # empty lot status falls back to the procurement
        self.assertEqual(rows[0]["cpv"], "45214200 · 71000000 · 71300000")
        self.assertEqual((rows[0]["amount"], rows[0]["currency"]), ("500", "EUR"))
        self.assertEqual(rows[0]["locator"], prov.csv_location(2))

    def test_rejects_foreign_download_host(self):
        meta = {"result": {"resources": [{"name": "x_2026.csv", "url": "https://elsewhere.example/file.csv"}]}}
        with patch.object(feeds, "js", return_value=meta), self.assertRaises(ValueError):
            collect(feeds.latvia("2026-11-19", "2026-11-19", None))


class RegistryTests(unittest.TestCase):
    def test_every_source_has_a_collector(self):
        self.assertEqual(set(feeds.SOURCES), set(feeds.COLLECTORS))
        for key in ("ro", "ch", "gr", "no", "lv"):
            self.assertEqual(len(feeds.SOURCES[key]), 5)


if __name__ == "__main__":
    unittest.main()
