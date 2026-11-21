import asyncio
from types import SimpleNamespace

from app.recipes.discovery import discover
from app.worker.runner import read


def test_discovery_uses_one_request_and_reads_cached_feed():
    class Reader:
        calls = []

        async def get(self, url, **kwargs):
            self.calls.append((url, kwargs))
            return SimpleNamespace(url=url, body=b"""<rss><channel><item>
                <guid>42</guid><title>Road works</title><cpvCodes>45000000-7</cpvCodes>
                </item></channel></rss>""")

    async def exercise():
        reader = Reader()
        url = 'https://example.org/procurement.xml'
        source, first = await discover(reader, url)
        tenders = await read(reader, source, first)
        assert reader.calls == [(url, {'redirects': False})]
        assert tenders['42']['cpvCodes'] == ['45000000']
        assert tenders['42']['buyerName'] is None

    asyncio.run(exercise())
