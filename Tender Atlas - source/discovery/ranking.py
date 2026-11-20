"""A small regularized logistic model ranks observed public links across languages."""

import json
import math
import re
from contextvars import ContextVar
from functools import lru_cache
from pathlib import Path
from urllib.parse import unquote, urlsplit

# Features describe destinations, without memorizing a publisher's hostname.
PATTERNS = [
    r"tender|notice|procurem|contract|march[eé]|aankondig|aanbested|appel.d.offre|cft|vergabe|licita|przetarg|zam.wie|avis",
    r"search|result|listing|overzicht|current|latest|opportunit|zoeken|recherche|busca",
    r"api|json|\.atom|rss|ocds|open.?data|dataset|data[/ -]|csv|publicatie|release|record",
    r"news|nieuws|actualit|press|presse|newsletter",
    r"about|help|faq|guidance|guide|handleiding|support|contact|manual|how.to|aide",
    r"privacy|cookies?|accessib|toegankelijk|legal|licence|terms|register|sign.in|login",
    r"detail|/\d{3,}|[a-f0-9]{8}-[a-f0-9]{4}",
]
compiled = [re.compile(p, re.I) for p in PATTERNS]
active_weights = ContextVar("portal_weights", default=None)
observed = ContextVar("portal_candidates", default=None)


def features(url, label=""):
    p = urlsplit(url)
    text = unquote(p.path + " " + p.query + " " + label).casefold()
    return [1.0] + [float(bool(rx.search(text))) for rx in compiled]


@lru_cache(maxsize=1)
def starter():
    return json.loads(
        Path(__file__).with_name("examples.json").read_text(encoding="utf-8")
    )


def fit(examples):
    # Equal total weight per publisher and class; hundreds of similar records
    # from one portal should not overwhelm a handful from another.
    groups = {}
    for item in examples:
        group = (item["portal"], int(item["label"]))
        groups.setdefault(group, set()).add(
            tuple(features(item["url"], item.get("text", "")))
        )
    weights = [0.0] * (1 + len(PATTERNS))
    for _ in range(140):
        gradient = [0.0] * len(weights)
        for (_, target), vectors in groups.items():
            for vector in vectors:
                z = sum(w * v for w, v in zip(weights, vector))
                error = (1 / (1 + math.exp(-z)) - target) / (len(groups) * len(vectors))
                for k, x in enumerate(vector):
                    gradient[k] += error * x
        weights = [w - 0.7 * (g + 0.025 * w) for w, g in zip(weights, gradient)]
    return weights


@lru_cache(maxsize=1)
def initial_weights():
    return fit(starter())


def score(url, label=""):
    weights = active_weights.get() or initial_weights()
    return sum(w * x for w, x in zip(weights, features(url, label)))


def remember(url, title):
    log = observed.get()
    if log is not None:
        log.setdefault(url, title)


def order(links):
    ranked = sorted(links, key=lambda pair: score(pair[1], pair[0]), reverse=True)
    log = observed.get()
    if log is not None:
        for title, url in ranked[:20]:
            log.setdefault(url, title)
    return ranked
