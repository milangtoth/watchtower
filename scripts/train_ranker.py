"""Fit Atlas-style weights using development labels and a probe JSON command.

Build the Rust example with cargo build --locked --example probe.
Usage: python scripts/train_ranker.py path/to/probe
Pass --container NAME to use an example binary inside a running Docker container.
Requires PyYAML. Output contains named coefficients in feature order.
"""

import argparse
import json
import math
import subprocess
from pathlib import Path
import sys
from urllib.parse import parse_qsl, urlsplit

import yaml

METHODS = ("html", "json", "ocds", "feed", "sitemap", "ted")
FIELDS = {"title", "deadline", "buyer", "publicationdate", "documents", "id"}
NAMES = ("bias", *METHODS, "http_2xx", "log_items", "coverage", "pagination",
         "same_host", "explicit_link")
INVENTORY = Path(__file__).resolve().parents[1] / "testset" / "portals.yaml"


def features(portal, candidate):
    seen = {field.lower() for field in candidate["fieldsSeen"]}
    if "submissiondeadline" in seen:
        seen.add("deadline")
    if "buyername" in seen:
        seen.add("buyer")
    return (
        1, *(candidate["method"].lower() == method for method in METHODS),
        200 <= candidate["httpStatus"] < 300, math.log1p(candidate["sampleItems"]),
        len(seen & FIELDS) / len(FIELDS), candidate["hasPagination"],
        urlsplit(portal).hostname == urlsplit(candidate["entryUrl"]).hostname,
        candidate["foundVia"].lower() in {"robots.txt", "link rel=alternate"},
    )


def load_portals():
    return yaml.safe_load(INVENTORY.read_text(encoding="utf-8"))["portals"]


def add_probe_arguments(parser):
    parser.add_argument("probe", help="path to the compiled Rust probe example")
    parser.add_argument("--container", help="run the probe binary in this Docker container")


def probe_results(args, rows, *options):
    if not rows:
        return
    urls = [row["url"] for row in rows]
    command = ["docker", "exec", args.container] if args.container else []
    if args.container and "--decide" in options:
        command[2:2] = ["-e", "OPENAI_API_KEY", "-e", "WATCHTOWER_MODEL"]
    output = subprocess.check_output(
        [*command, args.probe, "--json", *options, *urls], text=True, encoding="utf-8"
    )
    results = [json.loads(line) for line in output.splitlines() if line.strip()]
    if [result["url"] for result in results] != urls:
        raise ValueError("Probe results did not match the requested portal URLs in order")
    yield from zip(rows, results, strict=True)


def normalized_url(url):
    parts = urlsplit(url)
    port = parts.port
    if (parts.scheme, port) in {("https", 443), ("http", 80)}:
        port = None
    # Sorting by key preserves the order of repeated values for that key.
    query = tuple(sorted(parse_qsl(parts.query, keep_blank_values=True), key=lambda pair: pair[0]))
    return parts.scheme, parts.hostname, port, parts.path or "/", query


def usable_candidate(candidate):
    return 200 <= candidate["httpStatus"] < 300 or (
        candidate["method"] == "ted" and candidate["httpStatus"] == 0
    )


def is_labelled_candidate(row, candidate):
    return (
        row["expect"] == "serving"
        and candidate["method"] == row["best_method"]
        and usable_candidate(candidate)
        and normalized_url(candidate["entryUrl"]) == normalized_url(row["entry_url"])
    )


def fit(groups):
    if {target for _, target in groups} != {False, True}:
        raise ValueError("Training requires positive and negative development examples")
    weights = [0.0] * len(NAMES)
    for _ in range(140):
        gradient = [0.0] * len(weights)
        for (_, target), vectors in groups.items():
            for vector in sorted(vectors):
                z = sum(w * x for w, x in zip(weights, vector, strict=True))
                error = (1 / (1 + math.exp(-z)) - target) / (len(groups) * len(vectors))
                for i, x in enumerate(vector):
                    gradient[i] += error * x
        weights = [w - 0.7 * (g + 0.025 * w) for w, g in zip(weights, gradient)]
    return weights


def missing_entry_urls(results):
    return [
        {**row, "url": row["entry_url"]}
        for row, result in results
        if row["expect"] == "serving"
        and result.get("failure", {}).get("kind") not in {"blocked", "throttled"}
        and normalized_url(row["url"]) != normalized_url(row["entry_url"])
        and not any(is_labelled_candidate(row, c) for c in result.get("candidates", []))
    ]


def training_groups(results, supplemental):
    groups = {}
    discovered = seeded = 0
    for row, result in results:
        candidates = result.get("candidates", [])
        targets = [is_labelled_candidate(row, candidate) for candidate in candidates]
        if row["expect"] == "serving":
            if any(targets):
                discovered += 1
            else:
                direct = [
                    candidate for candidate in supplemental.get(row["entry_url"], [])
                    if is_labelled_candidate(row, candidate)
                ]
                if direct:
                    seeded += 1
                    candidates = candidates + direct
                    targets += [True] * len(direct)
            if not any(targets):
                failure = result.get("failure", {}).get("kind", "labelled route absent")
                print(f"{row['name']}: {failure}", file=sys.stderr)
                continue
        for candidate, target in zip(candidates, targets, strict=True):
            group = (row["url"], target)
            groups.setdefault(group, set()).add(features(row["url"], candidate))
    return groups, discovered, seeded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    add_probe_arguments(parser)
    args = parser.parse_args()
    rows = [
        row for row in load_portals()
        if row["split"] == "development" and (row["expect"] == "failed" or "best_method" in row)
    ]
    # Learn from the independently verified route when the normal probe misses it.
    # The evaluation only probes the original URL and still counts these as misses.
    results = list(probe_results(args, rows))
    supplemental = {
        row["url"]: result.get("candidates", [])
        for row, result in probe_results(args, missing_entry_urls(results))
    }
    groups, discovered, seeded = training_groups(results, supplemental)
    serving = sum(row["expect"] == "serving" for row in rows)
    print(f"Probe directly discovered {discovered}/{serving} labelled routes; "
          f"verified entry URLs supplied {seeded} additional training routes.", file=sys.stderr)
    print(json.dumps(dict(zip(NAMES, fit(groups), strict=True)), indent=2))


if __name__ == "__main__":
    main()
