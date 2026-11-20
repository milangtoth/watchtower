# Watchtower

Watchtower learns to read public procurement portals, publishes tenders and their documents, and attempts repair when a portal changes. Patterno runs the container and calls the HTTP interface defined in the [challenge brief](Patterno-Track-Description.pdf).

## Run

Copy `.env.example` to `.env` and set `OPENAI_API_KEY` and `WATCHTOWER_MODEL` for live attachment. Choose an OpenAI model available to your account that supports Structured Outputs.
Use unquoted values in a file passed directly to `docker run --env-file`.

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

The live Luna demo attached and repaired a two-tender mock portal. Each row below includes the verification call for that operation. These are estimates from recorded usage at the standard token rates. [GPT-6 Luna pricing](https://developers.openai.com/api/docs/models/gpt-6-luna#pricing)

| Operation | Calls | Uncached input tokens | Cached input tokens | Output tokens | Est. USD |
|---|---:|---:|---:|---:|---:|
| Attach and verify | 4 | 5,062 | 0 | 1,290 | $0.0011512 |
| Repair and verify | 4 | 6,096 | 0 | 860 | $0.0010396 |

Healthy repeat reads use the stored recipe without a model call. The cost to learn or repair a real portal varies with the evidence sampled and the number of attempts.

## Verify

Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`. The integration suite uses a local portal and a fake model to exercise paginated attachment, ZIP delivery, refresh deduplication, repair, access blocks, deletion, and restart recovery. Use `testset/portals.yaml` for real and held-out portal coverage.

Use `python3 scripts/check_contract.py --portal-id PORTAL_ID` for read-only checks against the running container. The `--exercise URL` mode also checks registration, refresh deduplication, and deletion using a fresh fixture URL.

To reproduce the live repair, [start the switchable mock](mock_portal/README.md) and run `python3 scripts/demo_repair.py` against a fresh fixture registration. The [55-second dashboard demo](docs/watchtower-demo.mp4) replays actual API snapshots from the verified Luna run.
