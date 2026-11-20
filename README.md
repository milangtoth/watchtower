# Watchtower

Watchtower learns to read public procurement portals, publishes tenders and their documents, and attempts repair when a portal changes. Patterno runs the container and calls the HTTP interface defined in the [challenge brief](Patterno-Track-Description.pdf).

## Run

Copy `.env.example` to `.env` and set `OPENAI_API_KEY` and `WATCHTOWER_MODEL` for live attachment. Choose an OpenAI model available to your account that supports Structured Outputs. Write values without quotes: Docker's `--env-file` passes quote characters through literally.

```sh
docker build --platform linux/amd64 -t watchtower .
docker run --rm -p 8080:8080 --env-file .env -v watchtower-data:/data watchtower
```

Open [the dashboard](http://localhost:8080/). Register a portal by supplying a public procurement listing URL.

```sh
curl -sS -X POST http://localhost:8080/portals \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://procurement.example/listings"}'
```

Use the returned ID with `GET /portals/{id}` to check progress, `GET /portals/{id}/tenders` to pull results, and each document's `path` to download its bytes. The service supports `?changedSince=` with an RFC 3339 timestamp and a URL-encoded offset.

For local development, export the key and model in your shell and run `cargo run --locked`. The HTTP server and dashboard can start without model credentials. Attach requests report missing model configuration in the portal's reason.

## Configuration

| Environment variable | Purpose |
|---|---|
| `OPENAI_API_KEY` | OpenAI credentials for learning, verification, and repair |
| `WATCHTOWER_MODEL` | OpenAI model ID with Structured Outputs support |
| `WATCHTOWER_DATA_DIR` | SQLite database and document blobs. Defaults to `./data` locally and `/data` in Docker. |
| `WATCHTOWER_READ_INTERVAL` | Seconds between scheduled reads. Defaults to `900`. |
| `RUST_LOG` | Service log filter. Defaults to `info`. |

## Tender assistant

Open **Tender assistant** from the operator dashboard. The assistant searches the published snapshots on `/portals/{id}/tenders` and shows source notices and available document links. Keyword search is available directly from the panel.

Set `ELEVENLABS_API_KEY` and `ELEVENLABS_AGENT_ID` in your runtime environment for live voice. The agent ID is in the ElevenAgents dashboard URL. Docker bundles the browser SDK. For local Cargo runs, build that asset first:

```sh
npm --prefix src/dashboard ci
npm --prefix src/dashboard run build
cargo run --locked
```

Use localhost or HTTPS for microphone access. Keep the voice session route private to trusted dashboard users when hosting the app.

For voice search, add an ElevenAgents **Client** tool named `search_tenders` and enable **Wait for response**. Configure these optional string parameters:

| Parameter | Use |
|---|---|
| `query` | Keywords from the notice, buyer, or reference |
| `country` | Two-letter country of performance, such as `NL` or `DE` |
| `cpv` | CPV code or prefix of two to eight digits |
| `deadline_from` | Earliest closing date, inclusive, in `YYYY-MM-DD` |
| `deadline_to` | Latest closing date, inclusive, in `YYYY-MM-DD` |

Tell the agent to search before answering tender questions, repeat the search when the caller changes a filter, and mention partial coverage. For cross-language comparisons, use a CPV code supplied by the caller or found in earlier results. Enable the `user_transcript`, `agent_response`, and `agent_response_correction` events for captions. Publish the agent settings.

## How it works

The Axum API places work in SQLite. The Tokio worker runs one operation per portal at a time. OpenAI uses bounded samples to propose a declarative recipe, which Rust validates before running it against HTML or public JSON. The same fetcher handles exploration, listing, detail, and document requests and spaces requests at least one second apart per portal. Downloads are streamed to disk and ZIP entries are unpacked and hashed.

A completed candidate is checked before publication. First reads, suspicious reads, and repairs use model verification. Healthy repeat reads use the saved recipe with no model calls. A repair runs against a new candidate while Watchtower serves the previous verified tenders and files. Successful repair replaces the recipe and snapshot in one transaction. The dashboard reads the same state and history as the API.

The reader handles linked HTML and JSON listings, detail pages, and tenders that combine direct downloads with nested attachment pages. Recipes specify the links to follow. If a file link returns an HTML attachment page, the reader looks for download links or a clear onward attachment link. Traversal has a limit of 32 attachment pages per tender and three levels of attachment-page nesting. Browser-only interfaces and navigation that requires submitting forms are outside this version.

## Model usage

SQLite records input, cached input, and output tokens for each model response, by portal, model, and operation. Use the query below on `watchtower.sqlite3` and multiply the totals by the selected model's rates per million tokens. Treat cached input separately from other input.

```sql
SELECT portal_id, model, operation, COUNT(*) AS calls,
       SUM(input_tokens - cached_input_tokens) AS uncached_input,
       SUM(cached_input_tokens) AS cached_input,
       SUM(output_tokens) AS output
FROM model_usage
GROUP BY portal_id, model, operation
```

Record a live attach and repair benchmark with the hand-in model before submission. Healthy repeat reads use zero model tokens.

## Verify

Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`. The integration suite uses a local portal and a fake model to exercise paginated attachment, ZIP delivery, refresh deduplication, repair, access blocks, deletion, and restart recovery. Use `testset/portals.yaml` for real and held-out portal coverage.
