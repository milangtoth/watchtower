import json
import re
from datetime import datetime
from app.fetch.details import resolve
from app.normalize.dates import deadline as parse_deadline, publication_date
from app.recipes.html import read_html
from app.recipes.json_listing import read_json
from urllib.parse import urljoin, urlsplit



from app import config
from app.fetch.documents import unpack
from app.recipes.discovery import name, xml


async def read(reader, source, first=None):
    if source.startswith("{"):
        recipe = json.loads(source)
        if "json" in recipe:
            return await read_json(reader, recipe, first)
        return await read_html(reader, recipe, first)
    pending = [(source, first)]
    seen = set()
    result = {}
    while pending:
        url, page = pending.pop()
        if url in seen:
            continue
        seen.add(url)
        page = page or await reader.get(url)
        root = xml(page.body)
        kind = name(root)
        if kind == 'sitemapindex':
            pending.extend((urljoin(page.url, node.text.strip()), None)
                           for node in root.iter() if name(node) == 'loc' and node.text)
            continue
        if kind not in {'rss', 'feed', 'urlset'}:
            raise ValueError('The saved path no longer returns a recognized XML source')
        if kind == 'urlset':
            feeds = [node.text.strip() for node in root.iter() if name(node) == 'loc'
                     and node.text and urlsplit(node.text.strip()).path.endswith(('.rss', '.atom', '.xml'))]
            if not feeds:
                raise ValueError('The sitemap lists webpages but exposes no XML tender feed')
            pending.extend((urljoin(page.url, target), None) for target in feeds)
            continue
        for item in root.iter():
            if name(item) not in {'item', 'entry'}:
                continue
            fields = {name(node): node.text.strip() for node in item if node.text and node.text.strip()}
            links = [node for node in item if name(node) == 'link']
            link = next((node.get('href') or node.text for node in links
                         if node.get('rel', 'alternate') == 'alternate'), None)
            key = fields.get('guid') or fields.get('id')
            tender_url = urljoin(page.url, link.strip()) if link else None
            identity = key or tender_url
            if not identity:
                raise ValueError('A feed entry has neither an identifier nor a detail link')
            if identity in result:
                continue
            if tender_url:
                detail, _ = await resolve(reader, tender_url, {'id': key, 'title': fields.get('title')})
                tender_url = detail.url
                identity = key or tender_url
            deadline = parse_deadline(fields.get('submissionDeadline') or fields.get('deadline'), page.url)
            publication = publication_date(fields.get('publicationDate') or fields.get('published') or fields.get('pubDate'))
            status = fields.get('status', '').lower()
            if status in {'withdrawn', 'cancelled', 'canceled'}:
                status = 'withdrawn'
            elif status == 'closed' or (deadline and datetime.fromisoformat(deadline) <= datetime.now().astimezone()):
                status = 'closed'
            else:
                status = 'open'
            documents = []
            for node in item:
                target = node.get('url') if name(node) == 'enclosure' else None
                if name(node) == 'link' and node.get('rel') == 'enclosure':
                    target = node.get('href')
                if target:
                    try:
                        file = await reader.get(urljoin(page.url, target), limit=config.MAX_FILE_BYTES)
                    except OverflowError:
                        continue
                    if 'text/html' in file.mime:
                        raise ValueError('Document returned an HTML page instead of file bytes')
                    documents.extend(unpack(urlsplit(file.url).path.rsplit('/', 1)[-1] or 'document', file.body))
            tender = dict(id=key, url=tender_url, title=fields.get('title'),
                          buyerName=fields.get('buyerName') or fields.get('buyer'),
                          publicationDate=publication,
                          submissionDeadline=deadline, status=status, documents=documents)
            for field in ('referenceNumber', 'description'):
                if field in fields:
                    tender[field] = fields[field]
            codes = re.findall(r'(?<!\d)\d{8}(?:-\d)?(?!\d)', fields.get('cpvCodes', ''))
            if codes:
                tender['cpvCodes'] = list(dict.fromkeys(code[:8] for code in codes))
            result[identity] = tender
    return result
