"""Exercise the provider change through discovery, verification, and saved-recipe replay."""
import asyncio
from json import dumps
from types import SimpleNamespace

import httpx
import pytest

from app import config
from app.recipes.openai_client import parse_answer
from app.worker.runner import read
from app.worker.scheduler import attach


LIST = "https://example.nl/open"
DETAIL = "https://example.nl/contract/42"
TITLE = "Road maintenance"


@pytest.mark.parametrize("kind", ["html", "json", "rss"])
def test_learning_and_replay(monkeypatch, kind):
    html = '<article><a href="/contract/42">Road maintenance</a></article>'
    if kind == "html":
        listing = html
        layout = {"rows": "article", "listing": {"url": "a@href", "title": "a"}}
        answers = [layout, layout]
    elif kind == "json":
        listing = dumps({"items": [{"name": TITLE, "link": DETAIL}]})
        answers = [{"records": "items", "fields": {"title": "name", "url": "link"}}]
    else:
        listing = f"<rss><channel><item><title>{TITLE}</title><link>{DETAIL}</link></item></channel></rss>"
        answers = []
    answers.append({"ok": True})
    pages = {LIST: listing, DETAIL: f"<h1>{TITLE}</h1><p>Contracting authority</p>"}

    class Publisher:
        async def get(self, url, **kwargs):
            return SimpleNamespace(url=url, body=pages[url].encode())

    async def mock_openai(client, url, *, headers, json):
        assert url == "https://api.openai.com/v1/chat/completions"
        assert headers["Authorization"] == "Bearer test-key"
        assert json["model"] == config.MODEL
        assert json["response_format"] == {"type": "json_object"}
        assert [m["role"] for m in json["messages"]] == ["developer", "user"]
        assert TITLE in json["messages"][1]["content"]
        assert answers, "Replaying a saved recipe unexpectedly contacted OpenAI"
        message = {"content": dumps(answers.pop(0))}
        return httpx.Response(200, json={"choices": [{"finish_reason": "stop", "message": message}]})

    monkeypatch.setattr(config, "OPENAI_API_KEY", "test-key")
    monkeypatch.setattr(httpx.AsyncClient, "post", mock_openai)

    async def exercise():
        reader = Publisher()
        recipe, tenders = await attach(reader, LIST)
        assert not answers
        assert tenders[DETAIL]["title"] == TITLE
        assert await read(reader, recipe) == tenders

    asyncio.run(exercise())


@pytest.mark.parametrize("choice", [
    {"finish_reason": "length", "message": {"content": "{}"}},
    {"finish_reason": "stop", "message": {"refusal": "declined"}},
    {"finish_reason": "stop", "message": {"content": "[]"}},
])
def test_unusable_responses_fail_closed(choice):
    with pytest.raises(ValueError):
        parse_answer({"choices": [choice]})
