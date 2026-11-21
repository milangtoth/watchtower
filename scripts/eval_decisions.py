"""Evaluate the live probe, Rust ranker and optional OpenAI decision.

Build with cargo build --locked --example probe, then run
python scripts/eval_decisions.py path/to/probe [--split development|holdout].
Set OPENAI_API_KEY and WATCHTOWER_MODEL for live decisions, or use --rank-only.
"""

import argparse
from collections import Counter
import os

from train_ranker import (
    add_probe_arguments,
    is_labelled_candidate,
    load_portals,
    probe_results,
    usable_candidate,
)


def probe_outcome(result):
    if "failure" in result:
        return result["failure"]["kind"]
    for candidate in result["candidates"]:
        if usable_candidate(candidate):
            return candidate["method"]
    return "no usable candidate"


def model_outcome(result):
    if "decision" in result:
        return result["decision"]["chosen"]["method"]
    if "failure" in result:
        return result["failure"]["kind"]
    return "decision failed"


def accuracy(actual, expected):
    return f"{actual}/{expected} ({actual / expected:.1%})" if expected else "-"


def report(split, results, use_model):
    scored = rank_hits = model_hits = method_found = route_found = 0
    blocked = blocked_hits = 0
    rank_confusion, model_confusion = Counter(), Counter()
    print(f"\n### {split.capitalize()}\n")
    print("| Portal | Expected | Rank | Model | Calls | Input | Cached input | Output |")
    print("| --- | --- | --- | --- | ---: | ---: | ---: | ---: |")
    for row, result in results:
        rank = probe_outcome(result)
        model = model_outcome(result) if use_model else "-"
        expected = row.get("best_method", "unlabelled")
        if row["expect"] == "failed":
            expected = "blocked"
            blocked += 1
            blocked_hits += rank == "blocked"
        elif expected != "unlabelled":
            scored += 1
            rank_hits += rank == expected
            model_hits += model == expected
            candidates = result.get("candidates", [])
            method_found += any(
                candidate["method"] == expected and usable_candidate(candidate)
                for candidate in candidates
            )
            route_found += any(is_labelled_candidate(row, candidate) for candidate in candidates)
            if rank != expected:
                rank_confusion[rank, expected] += 1
            if use_model and model != expected:
                model_confusion[model, expected] += 1
        usage = result.get("usage", {})
        name = row["name"].replace("|", "\\|")
        print(
            f"| {name} | {expected} | {rank} | {model} | {usage.get('calls', 0)} | "
            f"{usage.get('inputTokens', 0)} | {usage.get('cachedInputTokens', 0)} | "
            f"{usage.get('outputTokens', 0)} |"
        )
    print(f"\n- Expected method found by probe: {accuracy(method_found, scored)}")
    print(f"- Exact labelled route found by probe: {accuracy(route_found, scored)}")
    print(f"- Ranker top-1 accuracy: {accuracy(rank_hits, scored)}")
    if use_model:
        print(f"- Model choice accuracy: {accuracy(model_hits, scored)}")
    if blocked:
        print(f"- Expected access blocks detected: {accuracy(blocked_hits, blocked)}")
    for name, pairs in [("Ranker", rank_confusion), ("Model", model_confusion)]:
        for (actual, expected), count in pairs.most_common():
            print(f"- {name} chose {actual}, expected {expected} ({count})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    add_probe_arguments(parser)
    parser.add_argument("--split", choices=("development", "holdout"))
    parser.add_argument("--rank-only", action="store_true")
    args = parser.parse_args()
    use_model = bool(os.environ.get("OPENAI_API_KEY", "").strip()) and not args.rank_only
    if use_model and not os.environ.get("WATCHTOWER_MODEL", "").strip():
        parser.error("Set WATCHTOWER_MODEL alongside OPENAI_API_KEY or pass --rank-only")
    if not use_model:
        print("Rank-only evaluation. Accuracy excludes expected blocks and unlabelled portals.")
    else:
        print("Live model evaluation. Accuracy excludes expected blocks and unlabelled portals.")
    portals = load_portals()
    for split in [args.split] if args.split else ["development", "holdout"]:
        rows = [row for row in portals if row["split"] == split]
        report(split, probe_results(args, rows, "--decide" if use_model else "--rank"), use_model)


if __name__ == "__main__":
    main()
