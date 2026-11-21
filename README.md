# Watchtower

Watchtower turns a public procurement portal URL into a verified tender feed. It learns how to read the portal, serves tenders and their document bytes through the HTTP API in the [challenge brief](Patterno-Track-Description.pdf), and repairs itself when the portal changes. A read-only operator dashboard is served on `/`.

## Run

Copy `.env.example` to `.env` and set `OPENAI_API_KEY` and `WATCHTOWER_MODEL`. Write values without quotes, because Docker's `--env-file` passes quote characters through literally.

```sh
docker build --platform linux/amd64 -t watchtower .
docker run --rm -p 8080:8080 --env-file .env -v watchtower-data:/data watchtower
curl -sS -X POST http://localhost:8080/portals -H 'Content-Type: application/json' \
  -d '{"url":"https://procurement.example/listings"}'
```

Poll `GET /portals/{id}` for state, pull `GET /portals/{id}/tenders` (with `?changedSince=` for incremental pulls), and download each document from its `path` under `/files/{id}`. Refresh, run history and removal follow the appendix. For local development, run `cargo run --locked` with the same two variables exported.

| Environment variable | Purpose |
|---|---|
| `OPENAI_API_KEY` | Model credentials for deciding, learning, checking and repair. Required to attach. |
| `WATCHTOWER_MODEL` | OpenAI model ID with Structured Outputs support, such as `gpt-5.4`. Required to attach. |
| `WATCHTOWER_DATA_DIR` | SQLite database and document files. Defaults to `./data` locally and `/data` in Docker. |
| `WATCHTOWER_READ_INTERVAL` | Seconds between scheduled reads. Defaults to `900`. |
| `RUST_LOG` | Log filter. Defaults to `info`. |

The optional voice assistant's variables are in [docs/tender-assistant.md](docs/tender-assistant.md).

## How it works

One Rust service runs the API and dashboard (Axum), an in-process scheduler (Tokio), and SQLite for portals, runs, recipes and published tenders. Every request for a portal goes through one shared client that waits at least one second between requests.

1. **Attach.** `POST /portals` returns the ID at once and starts a background read. The portal is `integrating`.
2. **Probe and decide.** Cheap requests find candidate methods: HTML listing, public JSON, OCDS API, feed or sitemap. An embedded ranker scores them, and one model call picks one and records why.
3. **Learn.** The model proposes a declarative recipe from bounded page samples. Rust validates it and runs only the operations the recipe schema allows.
4. **Read and check.** The runner reads every listing page, detail page and document. It streams files to disk, unpacks ZIPs safely and skips files over 250 MB. It publishes only when pagination is shown complete and the fields and documents pass checks. First reads, suspicious reads and repairs also get a model check.
5. **Serve.** A verified read is published atomically and the portal is `serving`. Healthy repeat reads use the stored recipe with **no model calls**.
6. **Repair.** When a read breaks, the portal is `healing` and keeps serving its last verified tenders and files. The model gets the old recipe and the failure evidence, and the new recipe must pass the same checks before it replaces anything. When attempts run out, or a login or CAPTCHA blocks access, the portal is `failed` and the reason says why.

The dashboard shows each portal's state, reason, last success, tender count and links to tenders and files. Its run history pairs each broken read with the repair that fixed it. More detail is in [docs/architecture.md](docs/architecture.md).

## Model cost per portal

Token use is recorded per portal and operation in SQLite. The query is in [docs/architecture.md](docs/architecture.md). Measured on 2026-11-20 with `gpt-5.4` at $2.50 input, $0.25 cached input and $15 output per 1M tokens:

| Portal | Attach (learn and check) | Repair to date |
|---|---:|---:|
| Mock portal, 2 tenders | $0.017 | $0.018 (2 runs) |
| Fixture portal, 8 tenders | $0.049 | $0.096 |
| TenderNed (failed) | $0.057 | $0.381 |
| Find a Tender (failed, HTTP 403) | $0.079 | $0.022 |

A simple portal costs about two to five cents to attach. One full repair of the mock portal cost $0.076. Healthy repeat reads cost nothing. These figures predate the decide step, which adds one call per attach.

## Verify

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 scripts/check_contract.py --portal-id PORTAL_ID
```

The tests use a local mock portal and a fake model to cover pagination, ZIP delivery, refresh, repair, access blocks, deletion and restart recovery. Real portals are listed in [testset/portals.yaml](testset/portals.yaml), and the [mock portal](mock_portal/README.md) reproduces a live repair. The [demo video script](docs/demo-video-script.md) covers the hand-in video. Tender Atlas, a companion front end, is described in [docs/tender-atlas.md](docs/tender-atlas.md).
