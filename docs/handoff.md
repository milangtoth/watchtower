# Handoff: 2026-11-21, 05:30

Where the work stands, so a new session can continue without the old one. Read `AGENTS.md` first.

## Status

`main` builds and passes fmt, clippy and `cargo test --locked` (162 tests). **Claude Sonnet 5.5 is the model that works.** In the last live run (Claude, 7 portals, about $2.60), these were serving:

| Portal | Tenders | State |
|---|---:|---|
| Mock portal (`mock_portal`, port 8091) | 2 | serving |
| Plzeň Region E-ZAK (`ezak.cnpk.cz`) | 17 | serving |
| Berlin (`berlin.de/vergabeplattform`) | 247 | serving |
| Charles University E-ZAK | 0 | failed: one corrupt ZIP (`invalid Zip archive: Could not find EOCD`) fails the whole read |
| Masaryk University E-ZAK | 0 | failed: model check objected to a field (buyer) |
| Hradec Králové E-ZAK, GOV.UK | — | still attaching when the run was stopped |

## Model providers (`src/model.rs`)

Set in a root `.env`, which git ignores. Never commit keys.

| Provider | `.env` | Notes |
|---|---|---|
| Anthropic (use this) | `WATCHTOWER_MODEL_PROVIDER=anthropic`, `ANTHROPIC_API_KEY=…`, `WATCHTOWER_MODEL=claude-sonnet-5-5` | Structured outputs, effort `medium`, server-side refusal fallback on. Reliable recipes. |
| DeepSeek | `…=deepseek`, `DEEPSEEK_API_KEY`, `WATCHTOWER_MODEL=deepseek-chat` | Very cheap, but recipes vary run to run (wrong selectors, no paging). Not good enough. |
| OpenAI (default) | `OPENAI_API_KEY`, `WATCHTOWER_MODEL=gpt-5.4` | Works, but the credit ran out: four full runs cost $38. |

At most `WATCHTOWER_MODEL_CONCURRENCY` model calls run at once (default 6). 429 and 5xx are retried.

## Running a development read

```sh
cargo build --release --locked
# load .env into the shell, then:
WATCHTOWER_DATA_DIR=data/run1 WATCHTOWER_DEV_CACHE=data/dev-cache WATCHTOWER_DEV_RPS=5 ./target/release/watchtower
python scripts/live_run.py --base http://localhost:8080 --db data/run1/watchtower.sqlite3 --out data/run1/live-run.json
```

- `WATCHTOWER_DEV_CACHE` replays portal responses from disk, and `WATCHTOWER_DEV_RPS` (at most 5) raises the per-portal rate. Both are development only, off by default, and not set in Docker.
- `WATCHTOWER_CONCURRENCY` (default 32) is how many portals are worked on at once.
- `scripts/live_run.py` registers one split of `testset/portals.yaml` (default `development`) and prints state, tenders, documents and cost per portal. **Don't run all 29 again until the closest portals serve.** Attach single portals with `curl -X POST /portals` instead; it's cheaper and faster.
- On the old laptop, an old Docker container held `127.0.0.1:8080`. The microphone, and so the voice assistant, only works on `localhost` or HTTPS.

## Next steps, in order

1. **Corrupt ZIP:** Charles University. Store or skip an unreadable archive with a note instead of failing the read (`unpack` in `src/fetch.rs`, like #21 does for size limits).
2. Re-attach Charles, Masaryk, Hradec Králové and GOV.UK with Claude, read the model-check finding for each (the stored reason now names the failed check), and fix in code where it's generic.
3. One full run of the 29 development portals with Claude, for the README cost table and issue #18.
4. Hand-in: one-page README cost section (Claude numbers), a `linux/amd64` Docker build plus `scripts/check_contract.py`, a run on the 4 holdouts (BOAMP, Contracts Finder, Malta, Toruń), and the video of at most 3 minutes (mock portal repair demo, plus Plzeň and Berlin serving). The judges need a funded model key in the hand-in.

## Known gaps

- Portals that are JavaScript apps or use POST-only search APIs (Doffin, SEAP, KIMDIS, TED search, Prozorro, PNCP, In-tend, Bekanntmachungsservice) correctly end `failed`. There's no browser and no POST support.
- Masaryk E-ZAK: the model sometimes sets `page_parameter` without putting it in `start_url`.
- Voice in a native (non-Docker) build needs the `ELEVENLABS_*` variables and `npm --prefix src/dashboard ci && npm --prefix src/dashboard run build`.

## Merged today

#20–#50, including:
- The four phase 1 fixes: ZIP limit, OCDS window and E-ZAK, run reasons, attach fallback.
- Worker concurrency, dev cache, verify findings before outcomes, early stop, detail-page sampling, continuation rows, European dates and repeated IDs.
- DeepSeek and Anthropic providers.
- Teammates' PRs: probe, test set, SPA bundle discovery, login per route, header rows, Ireland filter.
