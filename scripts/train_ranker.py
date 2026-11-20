"""Fit Atlas-style weights using development labels and a probe JSON command.

Usage: python scripts/train_ranker.py path/to/probe
The executable accepts a portal URL and writes a JSON array of candidates.
Requires PyYAML. Output contains named coefficients in feature order.
"""

import argparse
import json
import math
import subprocess
from pathlib import Path
from urllib.parse import urlsplit

import yaml

METHODS = ("html", "json", "ocds", "feed", "sitemap", "ted")
FIELDS = {"title", "deadline", "buyer", "publicationdate", "documents", "id"}
NAMES = ("bias", *METHODS, "http_2xx", "log_items", "coverage", "pagination",
         "same_host", "explicit_link")


def features(portal, candidate):
    seen = {field.lower() for field in candidate["fields_seen"]}
    if "submissiondeadline" in seen:
        seen.add("deadline")
    if "buyername" in seen:
        seen.add("buyer")
    return (
        1, *(candidate["method"].lower() == method for method in METHODS),
        200 <= candidate["http_status"] < 300, math.log1p(candidate["sample_items"]),
        len(seen & FIELDS) / len(FIELDS), candidate["has_pagination"],
        urlsplit(portal).hostname == urlsplit(candidate["entry_url"]).hostname,
        candidate["found_via"].lower() in {"robots.txt", "link rel=alternate"},
    )


def fit(groups):
    if {target for _, target in groups} != {False, True}:
        raise ValueError("Training requires positive and negative development examples")
    weights = [0.0] * len(NAMES)
    for _ in range(140):
        gradient = [0.0] * len(weights)
        for (_, target), vectors in groups.items():
            for vector in vectors:
                z = sum(w * x for w, x in zip(weights, vector, strict=True))
                error = (1 / (1 + math.exp(-z)) - target) / (len(groups) * len(vectors))
                for i, x in enumerate(vector):
                    gradient[i] += error * x
        weights = [w - 0.7 * (g + 0.025 * w) for w, g in zip(weights, gradient)]
    return weights


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("probe")
    args = parser.parse_args()
    inventory = Path(__file__).resolve().parents[1] / "testset" / "portals.yaml"
    groups = {}
    for row in yaml.safe_load(inventory.read_text(encoding="utf-8"))["portals"]:
        if row["split"] != "development":
            continue
        blocked = row["expect"] == "failed"
        if not blocked and "best_method" not in row:
            continue
        url = row["url"]
        output = subprocess.check_output([args.probe, url], text=True, encoding="utf-8")
        for candidate in json.loads(output):
            target = (not blocked and candidate["method"].lower() == row["best_method"]
                      and 200 <= candidate["http_status"] < 300
                      and candidate["entry_url"] == row["entry_url"])
            groups.setdefault((url, target), set()).add(features(url, candidate))
    print(json.dumps(dict(zip(NAMES, fit(groups), strict=True)), indent=2))


if __name__ == "__main__":
    main()
