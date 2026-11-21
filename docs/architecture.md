# How Watchtower works

The [README](../README.md) has the short version. This page covers the full pipeline, the reader's limits and how model usage is measured.

## How it works

Watchtower turns a portal URL into a verified tender feed in eight steps. The model is used to decide, learn, check, and repair. Regular reads do not use the model.

| Step | What happens | Model |
|---|---|---|
| 1. **Attach** | `POST /portals` returns the portal ID at once and queues a background read. The portal is `integrating`. | — |
| 2. **Probe** | Cheap requests through the portal's rate-limited client find candidate connection methods: HTML listing, public JSON, OCDS API, RSS/Atom feed, sitemap, or TED. Each candidate records its entry URL, HTTP status, sample items, fields seen, and pagination. | — |
| 3. **Rank** | A small logistic-regression scorer with weights trained offline and embedded in Rust scores each candidate from 0 to 1. | — |
| 4. **Decide** | One model call picks a candidate and writes a rationale. The decision and the ranked alternatives are stored in SQLite and shown on the dashboard and by `GET /portals/{id}/connection`. | 1 call |
| 5. **Learn** | The model proposes a declarative recipe for the chosen method from bounded page or JSON samples. OCDS uses a fixed preset instead. Rust validates the recipe before running it. | a few calls |
| 6. **Read** | The recipe runner reads every listing page, the detail pages, and the documents. It streams files to disk and records evidence that pagination completed. | — |
| 7. **Check** | Pagination, field quality, documents, and source evidence are checked. First reads, suspicious reads, and repairs also get model verification. A verified read is published atomically, and the portal is `serving`. | 1 call when needed |
| 8. **Repair** | When a read breaks, the portal is `healing` and keeps serving its last verified tenders and files. The model gets the old recipe and the failure evidence. After two failed repair attempts on one method (one for OCDS), Watchtower re-decides with the next alternative (attempt 2). After two methods, the portal is `failed`, and the reason names the methods tried. A login or CAPTCHA also ends in `failed`. | per attempt |

Each attach probes the start page (and the home page when the start page is a JavaScript shell), sitemaps, feeds and TED links. Probing has no per-portal rules, only platform conventions such as the GOV.UK Content API. The ranker scores the candidates with embedded logistic regression weights. The decision call receives structured candidate evidence and chooses a usable route. If a repair exhausts one method, a new decision considers the remaining usable methods. [The test inventory](../testset/README.md) describes how to reproduce the evaluation.

The Axum API places work in SQLite. The Tokio worker runs one operation per portal at a time. OpenAI uses bounded samples to propose a declarative recipe, which Rust validates before running it against HTML or public JSON. The same fetcher handles exploration, listing, detail, and document requests and spaces requests at least one second apart per portal. Downloads are streamed to disk and ZIP entries are unpacked and hashed.

A completed candidate is checked before publication. First reads, suspicious reads, and repairs use model verification. Healthy repeat reads use the saved recipe with no model calls. A repair runs against a new candidate while Watchtower serves the previous verified tenders and files. Successful repair replaces the recipe and snapshot in one transaction. The dashboard reads the same state and history as the API.

The reader handles linked HTML and JSON listings, detail pages, and tenders that combine direct downloads with nested attachment pages. Recipes specify the links to follow. If a file link returns an HTML attachment page, the reader looks for download links or a clear onward attachment link. Traversal has a limit of 32 attachment pages per tender and three levels of attachment-page nesting. Browser-only interfaces and navigation that requires submitting forms are outside this version.

## Model usage

SQLite records input, cached input and output tokens for each model response. Run the report from the repository root to see estimated USD per portal and operation. It reads `WATCHTOWER_MODEL` or accepts an explicit model.

```sh
python scripts/model_costs.py data/watchtower.sqlite3 --model gpt-5.4
```

Use `--input-rate`, `--cached-input-rate`, and `--output-rate` for other models or pricing plans. Rates are in USD per million tokens.

