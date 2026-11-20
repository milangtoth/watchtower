# Watchtower

An agent that attaches to public-tender portals it has never seen, notices when one stops delivering, and repairs itself. Built for the Patterno challenge (brief: `Patterno-Track-Description.pdf`).

## Run

```bash
docker build -t watchtower .
docker run -p 8080:8080 --env-file .env watchtower
```

API and dashboard both on <http://localhost:8080>.

## Environment variables

| Variable | Purpose |
|---|---|
| `OPENAI_API_KEY` | OpenAI API key for recipe learning and sample verification |
| `WATCHTOWER_MODEL` | OpenAI model for recipe learning, checking and repair (default gpt-4.1-mini) |
| `WATCHTOWER_DATA_DIR` | SQLite database and downloaded documents |
| `WATCHTOWER_READ_INTERVAL` | Seconds between scheduled reads per portal |

## How it works

_TODO: one page. The idea: the LLM explores a portal once and writes a **recipe**; regular reads run the recipe with no model call; checks compare every read against the last good one; when checks fail the portal goes `healing` and the LLM repairs the recipe._

## Cost per portal

_TODO: tokens and price for one attach, one repair, and zero for a regular read._

## Layout

```
app/
  main.py              FastAPI app: API + dashboard + worker in one process
  config.py            Environment variables and limits
  api/                 The HTTP contract from the brief's appendix
    schemas.py           Response shapes (camelCase, exact field names)
    portals.py           /portals endpoints
    files.py             /files/{id} document bytes
  db/                  SQLite schema and repository helpers
  worker/              Background work
    scheduler.py         When to read, refresh, stop after DELETE
    runner.py            Runs a recipe with no model call
    states.py            integrating / serving / healing / failed
  fetch/               Talking to portals
    ratelimit.py         Max 1 request/second per portal
    http.py              Shared client
    browser.py           Headless browser, only if needed
    documents.py         Download, sha256, unzip, 250 MB limit
  recipes/             The LLM side
    schema.py            Recipe format
    llm.py               Jev candidate-link ranking
    openai_client.py     OpenAI JSON requests and token usage logging
    learn.py             Attach: write a recipe for an unseen portal
    repair.py            Repair: fix a recipe after a portal changed
  checks/              Is the delivery correct?
    fields.py            Per-field validation against the appendix
    health.py            Read-level health, login/captcha detection
  normalize/           Dates with offsets, CPV codes, enum mapping
  dashboard/           GET / for the operator
mock_portal/           Fake portal with switchable layouts for repair tests
testset/portals.yaml   Our real-portal test set
scripts/check_contract.py  Contract checker against a running instance
tests/
```

## Work split

| Area | Folders |
|---|---|
| Contract and core | `app/api`, `app/db`, `app/worker`, Dockerfile |
| Learning and repair | `app/recipes` |
| Fetching and data quality | `app/fetch`, `app/normalize`, `app/checks` |
| Test set, mock portal, dashboard | `testset`, `mock_portal`, `scripts`, `app/dashboard` |

## XML discovery and repair

Send a public listing URL to POST /portals. Discovery uses one GET with redirect
following disabled. It checks that response for an XML feed or advertised XML
links. Sitemap traversal, feed reads, and document downloads are separate
requests paced at one request per second per host. A landing URL that redirects
requires the final URL. Set TYPESAFE_API_KEY to let Jev rank ambiguous links.
Routine reads reuse the URL in SQLite and make no model calls.

When a saved URL fails, Watchtower records a repair and repeats discovery. It
keeps the previous snapshot until the replacement read succeeds. For HTML listings and direct JSON listings, set OPENAI_API_KEY and WATCHTOWER_MODEL to an compatible OpenAI chat model. Watchtower learns selectors or field paths and checks a sample tender against its source before publishing a new recipe. A refresh replays the saved recipe. JavaScript network discovery is currently unsupported. The Tender
Atlas JSONL is reference data and is not imported into the live API.

WATCHTOWER_OUTPUT_TIMEZONE controls the output timezone (default Etc/GMT-1, a fixed UTC+01:00). Dutch local deadlines use Europe/Amsterdam. Ambiguous clock-change times produce null unless the source supplies an offset. Publication dates retain their source calendar date.

## Run Tender Atlas with Watchtower

Copy .env.example to .env and configure your model credentials. From the repository root run
`docker compose up --build`. Open http://localhost:43871, choose **Portal operations**,
and connect a public listing URL. Atlas submits the URL to Watchtower and displays
its state, tenders, downloadable files, and run history. The existing Atlas explorer
and publisher collections use their own library. Existing legacy managed portals
can be connected again by URL to learn their recipes with Watchtower.

For a native installation run `python -m uvicorn app.main:app --port 8080` from
the repository root and start `python "Tender Atlas - source/atlas.py"` in another
terminal. Set WATCHTOWER_URL on the Atlas process if the worker uses another address.
