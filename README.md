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
| `ANTHROPIC_API_KEY` | Model key (handed in at submission) |
| `WATCHTOWER_MODEL` | Model used to learn, check and repair recipes |
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
    llm.py               Model client + cost accounting
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
