import asyncio
from types import SimpleNamespace

import pytest

from app.fetch.details import resolve
from app.normalize.dates import deadline
from app.recipes.html import read_html


@pytest.mark.parametrize('source,expected', [
    ('2026-07-01 10:00', '2026-07-01T09:00:00+01:00'),
    ('2026-12-01 10:00', '2026-12-01T10:00:00+01:00'),
    ('2026-10-25 02:30', None),
    ('2026-03-29 02:30', None),
    ('2026-10-25T02:30:00+02:00', '2026-10-25T01:30:00+01:00'),
])
def test_dutch_deadline(source, expected):
    assert deadline(source, 'https://test.nl') == expected


def test_summary_is_followed_on_replay():
    base = 'https://test.nl'
    pages = {
        '/list': '<article><a href="/summary">Road works</a></article>',
        '/summary': '<a href="/contract">Bekijk de aanbesteding</a>',
        '/contract': '<h1>Road works</h1><p>Aanbestedende dienst</p><time>01-07-2026 10:00</time>',
    }

    class Reader:
        async def get(self, url):
            return SimpleNamespace(url=url, body=pages[url.removeprefix(base)].encode())

    layout = {'rows': 'article', 'listing': {'url': 'a@href', 'title': 'a'},
              'detail': {'submissionDeadline': 'time'}}
    result = asyncio.run(read_html(Reader(), {'source': base + '/list', 'layout': layout}, None))
    assert result[base + '/contract']['submissionDeadline'] == '2026-07-01T09:00:00+01:00'
    with pytest.raises(ValueError, match='verify'):
        asyncio.run(resolve(Reader(), base + '/contract', {'title': 'Other procurement'}))
