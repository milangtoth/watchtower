"""Official procurement publishers. Each collector yields bounded batches of source records."""
import csv
import hashlib
import io
import itertools
import json
import re
import shutil
import ssl
import time
import zipfile
from datetime import date, datetime, timedelta
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urlencode, urljoin, urlsplit
from urllib.request import Request
from xml.etree import ElementTree as ET

import provenance as prov
from discovery import access, transport
from discovery.dates import clean_date

ROOT = Path(__file__).resolve().parent
TLS = ssl.create_default_context()
TLS.load_verify_locations(str(ROOT / "roots.pem"))
PUBLISHERS = transport.opener(TLS)
DAY = timedelta(days=1)
TED = "https://api.ted.europa.eu/v3/notices/search"
BOAMP = "https://www.boamp.fr/api/explore/v2.1/catalog/datasets/boamp/records"
FTS = "https://www.find-tender.service.gov.uk/api/1.0/ocdsReleasePackages"
NL = "https://www.tenderned.nl/cms/nl/aanbesteden-in-cijfers/datasets-aanbestedingen"
IE = "https://data.gov.ie/dataset/contract-notices-published-on-etenders"
ES = "https://contrataciondelsectorpublico.gob.es/sindicacion/sindicacion_643/licitacionesPerfilesContratanteCompleto3"
PL = "https://ezamowienia.gov.pl/mo-board/api/v1/notice"
SOURCES = {
    "ted": ("TED", "Tenders Electronic Daily", "European Union", "EU-wide notices, including planning, competitions, awards and changes. Full XML is linked from each record.", "https://docs.ted.europa.eu/ODS/latest/reuse/search-api.html"),
    "de": ("DÖE", "Bekanntmachungsservice", "Germany", "Daily federal procurement exports in OCDS format. The most recent available file covers yesterday.", "https://oeffentlichevergabe.de/ui/de/publicationPolicy"),
    "fr": ("BOAMP", "Bulletin officiel des annonces des marchés publics", "France", "DILA’s national and European notices, including awards and lower-value opportunities.", "https://www.boamp.fr/pages/donnees-ouvertes-et-api/"),
    "es": ("PLACSP", "Plataforma de Contratación del Sector Público", "Spain", "Contracting profiles hosted on the national platform, excluding minor contracts. Collected by update date, including the CODICE XML entry.", "https://contrataciondelsectorpublico.gob.es/wps/portal/DatosAbiertos"),
    "pl": ("BZP", "Biuletyn Zamówień Publicznych", "Poland", "National contract notices and results from e-Zamówienia, including the publisher’s HTML notice.", "https://ezamowienia.gov.pl/pl/integracja/"),
    "nl": ("TenderNed", "TenderNed", "Netherlands", "Historical OCDS releases from the published annual JSON files. Recent quarters appear after the archive is updated.", NL),
    "ie": ("eTenders", "eTenders open data", "Ireland", "Office of Government Procurement’s historical competitions and awards. Quarterly archive, indexed by notice, award or cancellation date. Rows without any of those dates are omitted.", IE),
    "uk": ("FTS", "Find a Tender", "United Kingdom", "UK procurement releases across all stages from the Cabinet Office. Collected by update date.", "https://www.gov.uk/government/publications/open-contracting"),
    "cf": ("CF", "Contracts Finder", "United Kingdom", "Cabinet Office daily OCDS exports with tender, award, buyer and supplier information.", "https://www.gov.uk/government/publications/open-contracting"),
}
COUNTRIES = json.loads((ROOT / "countries.json").read_text(encoding="utf8"))
TED_FIELDS = """publication-number publication-date notice-title title-proc buyer-name buyer-country form-type notice-type description-proc description-lot classification-cpv estimated-value-proc estimated-value-cur-proc result-value-notice result-value-cur-notice deadline-receipt-tender-date-lot deadline-receipt-request procedure-identifier internal-identifier-proc procedure-type winner-name place-of-performance document-url-lot buyer-internet-address official-language notice-version ojs-number""".split()

def get(url, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    opts = {"User-Agent":"TenderAtlas/1.0", "Accept":"*/*"}
    if body is not None:
        opts["Content-Type"] = "application/json"
    opts.update(headers or {})
    for attempt in range(4):
        try:
            with access.owner(url):
                return PUBLISHERS.open(Request(url, data=data, headers=opts), timeout=90)
        except HTTPError as exc:
            if exc.code not in (429, 500, 502, 503, 504) or attempt == 3:
                raise
            retry = exc.headers.get("Retry-After", "")
            exc.close()
            time.sleep(max(2**attempt, int(retry) if retry.isdigit() else 0))
        except (URLError, TimeoutError):
            if attempt == 3:
                raise
            time.sleep(2**attempt)

def fetch(url, body=None):
    with get(url, body) as response:
        return response.read()

def resource(url, body=None, discovered_from=None):
    with get(url, body) as response:
        content = response.read()
        document = prov.captured(url, response, body, discovered_from)
    document["response_sha256"] = hashlib.sha256(content).hexdigest()
    return content, document

def json_resource(url, body=None, discovered_from=None):
    content, document = resource(url, body, discovered_from)
    return json.loads(content), document

def js(url, body=None):
    return json.loads(fetch(url, body))

def query(url, **params):
    return url + "?" + urlencode(params)

def safe_next(url, base):
    url = urljoin(base, url)
    if urlsplit(url).scheme != "https" or urlsplit(url).hostname != urlsplit(base).hostname:
        raise ValueError("Publisher returned an unexpected download address.")
    return url

def flat(value):
    if isinstance(value, dict):
        for key in ("eng", "en"):
            if value.get(key):
                return flat(value[key])
        return flat(next((v for v in value.values() if v), ""))
    if isinstance(value, list):
        return " · ".join(dict.fromkeys(flat(v) for v in value if v is not None))
    return "" if value is None else str(value).strip()

def row(key, title, buyer, country, stage, published, url, raw, origin, *, description="", deadline="", cpv="", amount="", currency="", value_type="", process="", supplier=""):
    if not key or not published:
        raise ValueError("Publisher returned a record without an identifier or date.")
    return dict(ref=str(key), title=flat(title) or "Untitled notice", buyer=flat(buyer), country=country or "Unspecified",
                stage=stage, published=clean_date(published), url=url, raw=raw, origin=origin, description=flat(description),
                deadline=clean_date(deadline), cpv=flat(cpv), amount=flat(amount), currency=flat(currency), value_type=value_type,
                process=flat(process), supplier=flat(supplier))

def ocds(r, source, origin, version_id=None):
    t = r.get("tender") or {}
    awards = r.get("awards") or []
    tags = " ".join(r.get("tag") or []).lower()
    stage = ("Cancellation" if "cancellation" in tags else "Amendment" if "update" in tags or "amendment" in tags
             else "Award" if "award" in tags or "contract" in tags else "Planning" if "planning" in tags
             else "Tender" if "tender" in tags else "Other")
    rid = str(r["id"])
    if source == "de":
        url = "https://oeffentlichevergabe.de/api/notices/" + quote(rid) + "?format=pdf"
    elif source == "nl":
        url = "https://www.tenderned.nl/aankondigingen/overzicht/" + quote(rid)
    elif source == "uk":
        url = "https://www.find-tender.service.gov.uk/Notice/" + quote(rid)
    else:
        url = "https://www.contractsfinder.service.gov.uk/Notice/" + rid[:36]
    value = t.get("value") or {}
    kind = "Tender value"
    if len(awards) == 1 and (awards[0].get("value") or {}).get("amount") is not None:
        value, kind = awards[0]["value"], "Award value"
    codes = [t.get("classification") or {}] + (t.get("additionalClassifications") or [])
    for item in t.get("items") or []:
        codes += [item.get("classification") or {}] + (item.get("additionalClassifications") or [])
    cpvs = list(dict.fromkeys(str(c["id"]) for c in codes if c.get("id") and c.get("scheme","CPV").upper()=="CPV"))
    return row(version_id or rid, t.get("title") or next((a["title"] for a in awards if a.get("title")), ""),
               (r.get("buyer") or {}).get("name"), SOURCES[source][2], stage, r["date"], url, r, origin,
               description=t.get("description"), cpv=cpvs, deadline=(t.get("tenderPeriod") or {}).get("endDate"),
               amount=value.get("amount"), currency=value.get("currency"), value_type=kind, process=r.get("ocid"),
               supplier=[s.get("name") for a in awards for s in (a.get("suppliers") or []) if s.get("name")])

def ted(start, end, cache):
    payload = {"query":"publication-date = " + start.replace("-",""), "scope":"ALL", "fields":TED_FIELDS, "limit":100, "paginationMode":"ITERATION"}
    seen, count = set(), 0
    while True:
        data, document = json_resource(TED, payload)
        if data.get("timedOut"):
            raise RuntimeError("TED timed out while preparing the result. Resume this collection to retry.")
        rows = []
        for n in data["notices"]:
            v, cur, kind = n.get("estimated-value-proc"), n.get("estimated-value-cur-proc"), "Estimated value"
            if n.get("result-value-notice") is not None:
                v, cur, kind = n["result-value-notice"], n.get("result-value-cur-notice"), "Award value"
            ref = n["publication-number"]
            country = flat(n.get("buyer-country")).split(" · ")[0]
            country = COUNTRIES.get(country, country)
            stage = {"planning":"Planning","competition":"Tender","result":"Award","change":"Amendment","cont-modif":"Amendment","dir-awa-pre":"Direct award intent"}.get(flat(n.get("form-type")),"Other")
            rows.append(row(ref,n.get("title-proc") or n.get("notice-title"),n.get("buyer-name"),country,stage,n["publication-date"],
                "https://ted.europa.eu/en/notice/-/detail/"+ref,n,TED,description=n.get("description-proc") or n.get("description-lot"),
                deadline=n.get("deadline-receipt-tender-date-lot") or n.get("deadline-receipt-request"),cpv=n.get("classification-cpv"),
                amount=v,currency=cur,value_type=kind,process=n.get("procedure-identifier"),supplier=n.get("winner-name")))
        yield [prov.located(r, document, prov.json_location(f"/notices/{i}")) for i, r in enumerate(rows)]
        count += len(rows)
        if count >= data["totalNoticeCount"]:
            return
        token = data.get("iterationNextToken")
        if not rows or not token or token in seen:
            raise RuntimeError("TED stopped pagination before all matching notices arrived.")
        seen.add(token)
        payload["iterationNextToken"] = token
        time.sleep(.15)

def germany(start, end, cache):
    if start >= date.today().isoformat():
        return
    url = query("https://oeffentlichevergabe.de/api/notice-exports", pubDay=start, format="ocds.zip")
    archive, document = resource(url)
    if not archive:
        return
    with zipfile.ZipFile(io.BytesIO(archive)) as files:
        names = [f for f in files.namelist() if f.endswith(".json")]
        for group in itertools.batched(names, 100):
            batch = []
            for name in group:
                for i, r in enumerate(json.loads(files.read(name))["releases"]):
                    item = ocds(r, "de", url, Path(name).stem)
                    batch.append(prov.located(item, document, prov.json_location(f"/releases/{i}", name)))
            yield batch

def france(start, end, cache):
    offset = 0
    while True:
        where = "dateparution = date'" + start + "'"
        url = query(BOAMP, where=where, order_by="idweb", limit=100, offset=offset)
        result, document = json_resource(url)
        if result["total_count"] > 10000:
            url = query(BOAMP.replace("/records","/exports/json"),where=where)
            data, document = json_resource(url)
            prefix = ""
        else:
            data = result["results"]
            prefix = "/results"
        batch = []
        for n in data:
            stage = {"APPEL_OFFRE":"Tender","ATTRIBUTION":"Award","INTENTION_CONCLURE":"Direct award intent","PREINFORMATION":"Planning"}.get(n.get("nature"),"Other")
            if n.get("etat") == "RECTIFICATIF":
                stage = "Amendment"
            batch.append(row(n["idweb"],n.get("objet"),n.get("nomacheteur"),"France",stage,n["dateparution"],n["url_avis"],n,url,
                             deadline=n.get("datelimitereponse"),description=n.get("descripteur_libelle"),process=n.get("contractfolderid"),supplier=n.get("titulaire")))
        yield [prov.located(r, document, prov.json_location(f"{prefix}/{i}")) for i, r in enumerate(batch)]
        offset += len(data)
        if offset >= result["total_count"]:
            return
        if not data:
            raise ValueError("BOAMP ended the result set early.")
        time.sleep(.2)

def uk(start, end, cache):
    url = query(FTS,updatedFrom=start+"T00:00:00",updatedTo=end+"T23:59:59",limit=100)
    previous = set()
    discovered = None
    while url:
        if url in previous:
            raise ValueError("Find a Tender returned a repeated page.")
        previous.add(url)
        data, document = json_resource(url, discovered_from=discovered)
        yield [prov.located(ocds(r, "uk", url), document, prov.json_location(f"/releases/{i}"))
               for i, r in enumerate(data["releases"])]
        discovered = {"url": url, "json_pointer": "/links/next"}
        link = (data.get("links") or {}).get("next")
        url = safe_next(link,FTS) if link else None

def unflatten(fields):
    """Rebuild OCDS objects and indexed arrays from Contracts Finder's CSV paths."""
    root = {}
    for path, value in fields.items():
        if not value or not path or not path.startswith("releases/0/"):
            continue
        parts = path.split("/")[2:]
        obj = root
        for pos, part in enumerate(parts):
            leaf = pos == len(parts)-1
            new = value if leaf else ([] if parts[pos+1].isdigit() else {})
            if isinstance(obj, list):
                idx = int(part)
                while len(obj)<=idx:
                    obj.append(None)
                if obj[idx] is None:
                    obj[idx] = new
                obj = obj[idx]
            else:
                obj = obj.setdefault(part,new)
    return root

def cf(start, end, cache):
    url = "https://www.contractsfinder.service.gov.uk/harvester/Notices/Data/CSV/"+start.replace("-","/")
    content, document = resource(url)
    with io.TextIOWrapper(io.BytesIO(content), encoding="utf-8-sig", newline="") as stream:
        reader = csv.DictReader(stream)
        if reader.fieldnames and "releases/0/id" not in reader.fieldnames:
            raise ValueError("Contracts Finder returned an unexpected file.")
        for batch in itertools.batched(enumerate(reader, 1), 100):
            yield [prov.located(ocds(unflatten(n), "cf", url), document, prov.csv_location(i)) for i, n in batch]

def poland(start, end, cache):
    for typ, stage in [("ContractNotice","Tender"),("TenderResultNotice","Award"),("ContractPerformingNotice","Completion"),("NoticeUpdateNotice","Amendment"),("AgreementUpdateNotice","Amendment"),("CompetitionNotice","Tender"),("CompetitionResultNotice","Award"),("AgreementIntentionNotice","Direct award intent")]:
        params = dict(NoticeType=typ,PublicationDateFrom=start+"T00:00:00Z",PublicationDateTo=end+"T23:59:59.9999999Z",PageSize=100)
        while True:
            url = query(PL,**params)
            data, document = json_resource(url)
            rows = [(i, row(n["noticeNumber"],n.get("orderObject"),n.get("organizationName"),"Poland", stage,n["publicationDate"],
                         "https://ezamowienia.gov.pl/mo-client-board/bzp/notice-details/"+n["objectId"],n,url,
                         deadline=n.get("submittingOffersDate"),cpv=n.get("cpvCode"),process=n.get("tenderId"))) for i, n in enumerate(data) if start<=n["publicationDate"][:10]<=end]
            yield [prov.located(r, document, prov.json_location(f"/{i}")) for i, r in rows]
            if len(data)<100:
                break
            key = data[-1]["objectId"]
            if key == params.get("SearchAfter"):
                raise ValueError("BZP returned the same page twice.")
            params["SearchAfter"] = key
            time.sleep(3)
        time.sleep(3)

def cached(url, cache, suffix, discovered_from=None):
    access.ensure(url)
    cache.mkdir(parents=True,exist_ok=True)
    target = cache / (hashlib.sha256(url.encode()).hexdigest()+suffix)
    manifest = target.with_suffix(target.suffix + ".trace.json")
    if target.exists() and time.time()-target.stat().st_mtime < 86400:
        digest = prov.file_digest(target)
        try:
            document = json.loads(manifest.read_text(encoding="utf8"))
        except (OSError, ValueError):
            document = {}
        if isinstance(document, dict) and document.get("response_sha256") == digest and document.get("request", {}).get("url") == url:
            return target, document
        return target, prov.archive_evidence(url, digest)
    part = target.with_suffix(".part")
    manifest_part = manifest.with_suffix(".part")
    try:
        with get(url) as src, part.open("wb") as out:
            shutil.copyfileobj(src,out,1024*1024)
            size = src.headers.get("Content-Length")
            if size and out.tell()!=int(size):
                raise IOError("The archive download was interrupted.")
            document = prov.captured(url, src, discovered_from=discovered_from)
        document["response_sha256"] = prov.file_digest(part)
        part.replace(target)
        manifest_part.write_text(json.dumps(document), encoding="utf8")
        manifest_part.replace(manifest)
    finally:
        part.unlink(missing_ok=True)
        manifest_part.unlink(missing_ok=True)
    return target, document

def netherlands(start,end,cache):
    page = fetch(NL).decode()
    links = set(re.findall(r'href="([^"]+\.json)"',page,re.I))
    if not links:
        raise ValueError("TenderNed's archive catalogue did not list any JSON files.")
    for link in sorted(links):
        years = re.search(r'(\d{4}-\d\d-\d\d)[_-](\d{4}-\d\d-\d\d)',link)
        if years and (years[2]<start or years[1]>end):
            continue
        url = safe_next(link, NL)
        path, document = cached(url,cache,".json", {"url": NL, "link_href": link})
        with path.open(encoding="utf-8-sig") as f:
            for group in itertools.batched(enumerate(iter_releases(f)),150):
                yield [prov.located(ocds(r,"nl",url), document, prov.json_location(f"/releases/{i}"))
                       for i,r in group if start<=r["date"][:10]<=end]

def iter_releases(stream):
    buf = ""
    match = None
    while match is None:
        chunk = stream.read(65536)
        if not chunk:
            raise ValueError("Missing OCDS release list.")
        buf += chunk
        match = re.search(r'"releases"\s*:\s*\[',buf)
    buf = buf[match.end():]
    decoder = json.JSONDecoder()
    while True:
        buf = buf.lstrip(" \r\n\t,")
        if buf.startswith("]"):
            return
        try:
            obj, used = decoder.raw_decode(buf)
        except json.JSONDecodeError:
            chunk = stream.read(262144)
            if not chunk:
                raise ValueError("The OCDS archive ended unexpectedly.")
            buf += chunk
            continue
        yield obj
        buf = buf[used:]

def irish_date(value):
    if not value:
        return ""
    for fmt in ("%d/%m/%Y","%Y-%m-%d","%d/%m/%Y %H:%M"):
        try:
            return datetime.strptime(value.strip(),fmt).date().isoformat()
        except ValueError:
            pass
    raise ValueError("Unrecognised date in the eTenders archive: "+value)

def ireland(start,end,cache):
    catalogue = "https://data.gov.ie/api/3/action/package_show?id=contract-notices-published-on-etenders"
    meta = js(catalogue)["result"]
    resources = [r for r in meta["resources"] if r["format"].lower().strip(".")=="csv"]
    if not resources:
        raise ValueError("The eTenders catalogue did not list its CSV archive.")
    for res in resources:
        url = res["url"]
        if urlsplit(url).scheme!="https" or not urlsplit(url).hostname.endswith(".gov.ie"):
            raise ValueError("The eTenders archive has an unexpected download address.")
        path, document = cached(url,cache,".csv", {"url": catalogue, "json_pointer": f"/result/resources/{meta['resources'].index(res)}/url"})
        with path.open("rb") as f:
            head = f.readline()
        try:
            head.decode("utf-8-sig")
            encoding = "utf-8-sig"
        except UnicodeDecodeError:
            encoding = "cp1252"
        with path.open(encoding=encoding,errors="replace",newline="") as f:
            reader = csv.DictReader(f)
            for group in itertools.batched(enumerate(reader,1),200):
                batch = []
                for number, r in group:
                    dt = irish_date(r["Notice Published Date / Contract Created Date"] or r.get("Award Published") or r.get("Cancelled Date"))
                    if not start<=dt<=end:
                        continue
                    ref = r["Tender ID"]
                    # The archive can contain several undifferentiated award rows per tender.
                    # A content fingerprint retains every distinct published row on repeat imports.
                    digest = json.dumps(r,ensure_ascii=False,sort_keys=True,separators=(",",":")).encode()
                    key = ref+"-"+hashlib.sha256(digest).hexdigest()[:20]
                    av = r.get("Sum of Awarded Value (€)")
                    val = av or r.get("Sum of Notice Estimated Value (€)")
                    stage = "Cancellation" if r.get("Cancelled Date") else "Award" if r.get("Award Published") or av else "Tender"
                    record = row(key,r["Tender/Contract Name"],r["Contracting Authority"],"Ireland",stage,dt,
                        r.get("TED CAN Link") or r.get("TED Notice Link") or (("https://www.etenders.gov.ie/epps/cft/prepareViewCfTWS.do?resourceId="+quote(ref)) if r.get("Platform")=="ED Platform" else IE),
                        r,url,deadline=irish_date(r["Tender Submission Deadline"]),description=r.get("Main Cpv Code Description"),
                        cpv=r.get("Additional CPV Codes on CFT") or r.get("Main Cpv Code"),amount=val,currency="EUR" if val else "",
                        value_type="Award value" if av else "Estimated value",process=ref,supplier=r.get("Awarded Suppliers"))
                    batch.append(prov.located(record,document,prov.csv_location(number)))
                if batch:
                    yield batch

def es_page(root,start,end,url,document,member=None):
    if root.tag.split("}")[-1]!="feed":
        raise ValueError("Spain's publisher returned a page without an Atom feed.")
    dates, rows = [], []
    for number,e in enumerate(root.findall("{*}entry"),1):
        updated = e.findtext("{*}updated") or ""
        dates.append(updated[:10])
        if not start<=updated[:10]<=end:
            continue
        folder = e.find("{*}ContractFolderStatus")
        if folder is None:
            raise ValueError("Spanish feed entry has no contract folder.")
        project = folder.find("{*}ProcurementProject")
        val = project.find("{*}BudgetAmount/{*}EstimatedOverallContractAmount") if project is not None else None
        code = folder.findtext("{*}ContractFolderStatusCode")
        stage = {"PUB":"Tender","PRE":"Planning","ADJ":"Award","RES":"Award","ANUL":"Cancellation","EV":"Evaluation"}.get(code,"Other")
        key = e.findtext("{*}id").rsplit("/",1)[-1]+"@"+updated
        record = row(key,e.findtext("{*}title"),folder.findtext("{*}LocatedContractingParty/{*}Party/{*}PartyName/{*}Name"),"Spain",stage,
                    updated,e.find("{*}link").get("href"),{"xml":ET.tostring(e,encoding="unicode")},url,description=e.findtext("{*}summary"),
                    deadline=folder.findtext("{*}TenderingProcess/{*}TenderSubmissionDeadlinePeriod/{*}EndDate"),
                    cpv=[n.text for n in folder.findall("{*}ProcurementProject/{*}RequiredCommodityClassification/{*}ItemClassificationCode")],
                    amount=val.text if val is not None else "",currency=val.get("currencyID") if val is not None else "",value_type="Estimated value",
                    process=folder.findtext("{*}ContractFolderID"))
        rows.append(prov.located(record,document,prov.atom_location(number,member)))
    return rows, dates

def spain(start,end,cache):
    if start[:7] < date.today().isoformat()[:7]:
        url = ES+"_"+start[:7].replace("-","")+".zip"
        path, document = cached(url, cache, ".zip")
        with zipfile.ZipFile(path) as archive:
            for name in archive.namelist():
                if name.endswith(".atom"):
                    root = ET.fromstring(archive.read(name))
                    yield es_page(root,start,end,url,document,name)[0]
        return
    url = ES+".atom"
    discovered = None
    seen = set()
    while url:
        if url in seen:
            raise ValueError("Spain's feed repeated a page.")
        seen.add(url)
        content, document = resource(url, discovered_from=discovered)
        root = ET.fromstring(content)
        batch, dates = es_page(root,start,end,url,document)
        yield batch
        if dates and min(dates)<start:
            return
        link = root.find("{*}link[@rel='next']")
        if link is None:
            break
        name = urlsplit(link.get("href","")).path.rsplit("/",1)[-1]
        if not re.fullmatch(r'licitacionesPerfilesContratanteCompleto3_[\d_]+\.atom',name):
            raise ValueError("Unrecognised archive link in the Spanish feed.")
        discovered = {"url": url, "link_rel": "next", "link_href": link.get("href")}
        url = urljoin(ES,name)
        time.sleep(.2)

COLLECTORS = dict(ted=ted,de=germany,fr=france,es=spain,pl=poland,nl=netherlands,ie=ireland,uk=uk,cf=cf)
