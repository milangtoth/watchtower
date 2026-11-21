"""Replay learned HTML selectors without calling the model during refreshes."""
import json
from datetime import datetime
from urllib.parse import urljoin, urlsplit

from bs4 import BeautifulSoup
from pydantic import BaseModel, Field

from app import config
from app.fetch.details import resolve
from app.fetch.documents import unpack
from app.normalize.dates import deadline, publication_date
from app.recipes.learn import learn


class Layout(BaseModel):
    rows: str = Field(min_length=1)
    listing: dict[str, str]
    detail: dict[str, str] = Field(default_factory=dict)
    documents: str | None = None
    next: str | None = None
    timezone: str | None = None
    detailEvidence: str | None = None


def extract(node, selectors):
    values = {}
    for field, rule in selectors.items():
        selector, separator, attribute = rule.partition('@')
        match = node if selector == ':scope' else node.select_one(selector)
        if match is not None:
            value = match.get(attribute) if separator else match.get_text(' ', strip=True)
            if isinstance(value, str) and value.strip():
                values[field] = value.strip()
    return values


async def learn_html(reader, page):
    layout = Layout.model_validate(await learn(page.body))
    soup = BeautifulSoup(page.body, 'html.parser')
    row = soup.select_one(layout.rows)
    fields = extract(row, layout.listing) if row else {}
    if not fields.get('url'):
        raise ValueError('Learned listing has no tender detail link')
    detail = await reader.get(urljoin(page.url, fields['url']))
    layout = Layout.model_validate(await learn(page.body, detail.body))
    return json.dumps({'source': page.url, 'layout': layout.model_dump()}), page


async def download_documents(reader, detail, body, selector):
    docs = []
    for link in body.select(selector) if selector else []:
        try:
            file = await reader.get(urljoin(detail.url, link['href']), limit=config.MAX_FILE_BYTES)
        except OverflowError:
            continue
        if 'html' in file.mime:
            raise ValueError('Document link returned a webpage')
        docs.extend(unpack(urlsplit(file.url).path.rsplit('/', 1)[-1] or 'document', file.body))
    return docs


async def read_html(reader, recipe, first):
    layout = Layout.model_validate(recipe['layout'])
    url = recipe['source']
    seen = set()
    result = {}
    while url:
        if url in seen:
            raise ValueError('Pagination returned a previously visited page')
        seen.add(url)
        page = first or await reader.get(url)
        first = None
        soup = BeautifulSoup(page.body, 'html.parser')
        rows = soup.select(layout.rows)
        if not rows:
            raise ValueError('The learned listing selector found no tenders')
        for row in rows:
            fields = extract(row, layout.listing)
            if not fields.get('url'):
                raise ValueError('Tender detail link disappeared')
            detail, body = await resolve(reader, urljoin(page.url, fields['url']), fields, layout.detailEvidence)
            fields.update(extract(body, layout.detail))
            due = deadline(fields.get('submissionDeadline'), detail.url, layout.timezone)
            status = (fields.get('status') or '').casefold()
            if status in ('withdrawn', 'cancelled', 'ingetrokken'):
                status = 'withdrawn'
            elif status == 'closed' or (due and datetime.fromisoformat(due) < datetime.now().astimezone()):
                status = 'closed'
            else:
                status = 'open'
            docs = await download_documents(reader, detail, body, layout.documents)
            tender = {key: fields.get(key) for key in ('id', 'title', 'buyerName')}
            tender.update(url=detail.url, documents=docs, status=status, submissionDeadline=due,
                          publicationDate=publication_date(fields.get('publicationDate')))
            for key in ('description', 'referenceNumber'):
                if key in fields:
                    tender[key] = fields[key]
            identity = tender['id'] or detail.url
            if identity in result:
                raise ValueError('Listing entries resolved to a duplicate tender')
            result[identity] = tender
        following = soup.select_one(layout.next) if layout.next else None
        url = urljoin(page.url, following['href']) if following else None
    return result
