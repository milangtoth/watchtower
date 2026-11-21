"""Print Watchtower's per-portal model costs from an existing SQLite database.

Set WATCHTOWER_MODEL or pass --model. Rates are in USD per million tokens and
default to the published standard GPT-5.4 and GPT-5.4 mini rates.
"""

import argparse
from contextlib import closing
from decimal import Decimal, InvalidOperation
import os
from pathlib import Path
import sqlite3


# https://developers.openai.com/api/docs/models/gpt-5.4
# https://developers.openai.com/api/docs/models/gpt-5.4-mini
STANDARD_RATES = {
    "gpt-5.4": (Decimal("2.50"), Decimal("0.25"), Decimal("15.00")),
    "gpt-5.4-mini": (Decimal("0.75"), Decimal("0.075"), Decimal("4.50")),
}
MILLION = Decimal(1_000_000)

USAGE_BY_PORTAL = """
SELECT COALESCE(p.url, u.portal_id), u.operation, COUNT(*),
       SUM(u.input_tokens), SUM(u.cached_input_tokens), SUM(u.output_tokens)
FROM model_usage AS u
LEFT JOIN portals AS p ON p.id = u.portal_id
WHERE u.model = ?
GROUP BY u.portal_id, p.url, u.operation
ORDER BY p.url, u.portal_id, u.operation
"""


def rate(value):
    try:
        parsed = Decimal(value)
    except InvalidOperation as error:
        raise argparse.ArgumentTypeError("rate must be a decimal number") from error
    if not parsed.is_finite() or parsed < 0:
        raise argparse.ArgumentTypeError("rate must be finite and nonnegative")
    return parsed


def estimate(input_tokens, cached_input, output_tokens, rates):
    if not 0 <= cached_input <= input_tokens or output_tokens < 0:
        raise ValueError("SQLite usage contains inconsistent token counts")
    input_rate, cached_rate, output_rate = rates
    return (
        (input_tokens - cached_input) * input_rate
        + cached_input * cached_rate
        + output_tokens * output_rate
    ) / MILLION


def print_report(database, model, rates):
    # Use SQLite's read-only mode so a bad path cannot silently create an empty database.
    with closing(sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)) as db:
        rows = db.execute(USAGE_BY_PORTAL, (model,)).fetchall()
    if not rows:
        print(f"No usage recorded for {model} in {database}.")
        return

    print(f"Estimated USD for {model}")
    print(f"Rates per million tokens  input ${rates[0]}  cached input ${rates[1]}  output ${rates[2]}")
    print()
    print("| Portal | Operation | Calls | Input | Cached input | Output | Estimated USD |")
    print("| --- | --- | ---: | ---: | ---: | ---: | ---: |")
    total_calls = total_input = total_cached = total_output = 0
    total_cost = Decimal(0)
    for portal, operation, calls, input_tokens, cached_input, output_tokens in rows:
        cost = estimate(input_tokens, cached_input, output_tokens, rates)
        total_calls += calls
        total_input += input_tokens
        total_cached += cached_input
        total_output += output_tokens
        total_cost += cost
        portal = portal.replace("|", "\\|")
        print(f"| {portal} | {operation} | {calls:,} | {input_tokens:,} | "
              f"{cached_input:,} | {output_tokens:,} | ${cost:,.6f} |")
    print(f"| **Total** | | {total_calls:,} | {total_input:,} | "
          f"{total_cached:,} | {total_output:,} | **${total_cost:,.6f}** |")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    default_db = Path(os.environ.get("WATCHTOWER_DATA_DIR", "data")) / "watchtower.sqlite3"
    parser.add_argument("db", type=Path, nargs="?", default=default_db,
                        help="path to an existing watchtower.sqlite3 database")
    parser.add_argument("--model", default=os.environ.get("WATCHTOWER_MODEL", ""),
                        help="model ID as recorded in SQLite (defaults to WATCHTOWER_MODEL)")
    parser.add_argument("--input-rate", type=rate, help="USD per million uncached input tokens")
    parser.add_argument("--cached-input-rate", type=rate, help="USD per million cached input tokens")
    parser.add_argument("--output-rate", type=rate, help="USD per million output tokens")
    args = parser.parse_args()
    model = args.model.strip()
    if not model:
        parser.error("set WATCHTOWER_MODEL or pass --model")
    rates = (args.input_rate, args.cached_input_rate, args.output_rate)
    if all(value is None for value in rates):
        rates = STANDARD_RATES.get(model)
    if rates is None or any(value is None for value in rates):
        parser.error("pass all three rates for a custom price or an unlisted model")
    try:
        print_report(args.db, model, rates)
    except (OSError, sqlite3.Error, ValueError) as error:
        parser.exit(1, f"Could not read Watchtower usage: {error}\n")


if __name__ == "__main__":
    main()
