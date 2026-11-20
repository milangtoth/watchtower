# Watchtower

Watchtower is a service for public procurement portals. Given a portal URL, it learns how to read it, delivers tenders and their documents via an API, and recovers when the portal changes. The operator dashboard shows delivery status and run history.

[The Patterno challenge brief](Patterno-Track-Description.pdf), especially the appendix, is the source of truth for judging and the API.

## Confirmed decisions

- Build Watchtower in **Rust**. The Rust runtime lives in `src/`. The earlier scaffold is a reference.
- Prioritize the Watchtower service and its end-to-end API flow. Use existing public procurement portals as development inputs. The mock portal is a secondary fixture for repeatable failures and repair tests. Its setup can proceed in parallel.

## Architecture

The first implementation uses Axum, Tokio, SQLite, and OpenAI. Set the exact model ID for your account before a live run.

- Use one Cargo package with Axum for the API and dashboard and Tokio for the in-process scheduler. Start and stop the scheduler with the app lifecycle. Keep the API responsive during portal work.
- Use SQLite via SQLx for portals, runs, recipes, and published tenders. Store document bytes under `WATCHTOWER_DATA_DIR` and keep their metadata in SQLite. Stage files before committing a snapshot that references them.
- Use a shared `reqwest` client for listing, detail, document, and model exploration requests. Route redirects and retries through the same rate limiter. Use bounded timeouts and report access blocks when they prevent delivery.
- Start with server-rendered HTML and public JSON. Revisit browser support when a representative test portal needs it and browser requests can obey the rate limit.
- Keep OpenAI Responses API calls in the model module. Supply `OPENAI_API_KEY` and `WATCHTOWER_MODEL` through the environment. Record input, cached input, and output tokens per portal and operation.
- Serve a simple HTML dashboard on `/`. Show state, reason, last success, tender count, recent reads and repairs, and links to tenders and downloaded files. The dashboard is read-only. Use the API for portal registration and refresh.

## Shared interfaces

| Surface | Owner | What it passes across the boundary |
|---|---|---|
| `recipes` | Learning and repair with fetching | A validated, declarative Rust recipe type for listing, pagination, details, field extraction, and document navigation. Start with CSS selectors and JSON field paths. |
| `fetch`, `normalize`, recipe runner | Fetching | A candidate read with normalized tenders, staged document references, evidence of listing and pagination completeness, and collection or access failures. |
| `recipes`, `checks` | Learning and repair | A learned or repaired recipe and a check verdict with a human-readable reason and supporting evidence. |
| `worker`, `db`, `api`, `Dockerfile` | Core | Scheduling, run IDs and history, portal state transitions, atomic publication of verified results, and the submission build. |
| Mock portal, `testset`, contract checks, `dashboard` | Verification and dashboard | Fixtures with independent expected results, contract checks, real-portal coverage, and the operator view. |

Agree on one recipe example, one candidate read example, and one check result before implementing both sides. Keep API response types in the `api` module and keep fetching and validation independent of API handlers. The worker connects the parts.

## Read, check, and repair flow

1. **Attach.** Return the portal ID immediately from `POST /portals`. Start a background `read` run in `integrating`. Give the LLM bounded access to page and JSON samples through the shared fetcher. Treat fetched content as untrusted data. Validate the proposed recipe and execute it with the same runner used for subsequent reads. Execute only operations defined in the recipe schema.
2. **Check.** Verify pagination, field quality, documents, and source evidence. Use LLM verification on first attach, suspicious reads, and repaired candidates. Keep healthy repeat reads deterministic. Require evidence of complete pagination even when HTTP status and tender counts look normal.
3. **Publish.** Commit a verified read atomically and report `serving`. Keep the portal's own tender identifiers. Update `lastChangedAt` only for actual changes. Apply closure logic after a verified complete read, and preserve reported cancellations as `withdrawn`.
4. **Repair.** When evidence shows delivery broke, enter `healing` and preserve the last verified tenders and files. Record a separate repair run. Give the model the old recipe and failure evidence, then execute and verify the new candidate before replacing either the active recipe or the snapshot. Bound attempts and record a specific `failed` reason if they are exhausted. A login or captcha that prevents delivery also ends in `failed` with the blocker in the reason.

Use a shared per-portal scheduling guard for attach, periodic reads, refresh, and repair. Return the active read ID for a concurrent refresh. Deletion must stop portal requests within one minute and prevent in-flight work from publishing after removal. Keep per-portal errors isolated from other portals.

## Patterno integration and challenge constraints

- Patterno runs the submitted container and calls Watchtower's HTTP API on port 8080. The challenge API has no authentication. Patterno supplies portal URLs. Watchtower fetches the source data and serves normalized tenders and file bytes from its own API.
- The judge can register a portal with `POST /portals`, poll `GET /portals/{id}`, pull tenders with `GET /portals/{id}/tenders`, and download files from the paths in each tender. `changedSince` supports incremental pulls. The API also exposes refresh, run history, and removal. The dashboard is served at `/`.
- Preserve exact appendix routes, status codes, response shapes, and casing. Repeated registration of the same URL returns the same ID. Unknown IDs return 404. `changedSince` is strictly later than the supplied timestamp.
- Keep serving the last correct tenders while a portal is `healing`. Report a login or captcha that prevents delivery as `failed`, with the blocker in the reason.
- Required fields the portal does not show are JSON `null` where the appendix calls for it. Omit absent optional fields. Preserve timezone offsets on date-times and leave uncertain source values missing. Use eight-digit CPV codes without check digits.
- Serve document bytes at `/files/{id}` with matching file name, size, and SHA-256 in tender metadata. Stream downloads, unpack ZIPs safely, and apply the brief's 250 MB exception to files and archives.
- Operate autonomously during judging. Use an LLM to learn, check, and repair. Allow at most one request per second per portal. Use public pages and downloads. The brief prohibits logins and captcha solving.
- The submission needs a root Dockerfile that builds for `linux/amd64`, a README describing the system, environment, and measured model cost per portal, and a dashboard demo lasting at most three minutes. Provide required model credentials through the challenge hand-in.

## Proposed team workflow and verification

- **Core and integration:** project lead. **Fetching**, **learning and repair**, and **verification and dashboard:** one teammate each. Coordinate changes to both sides of a shared interface with its listed owners.
- Use short, focused branches and PRs to `main`. Request one teammate review for changes to a shared interface. Merge the shared interface examples early so implementation can proceed in parallel.
- Make the first end-to-end milestone a Rust service that accepts a public portal URL, starts a background read, and serves verified tenders and their document bytes through the API. A handwritten recipe can unblock runner integration. Repeat from a clean start with URL-only model learning before counting attachment as complete.
- Give the mock portal a layout change that still returns HTTP 200 and breaks the old recipe. Use it to verify `serving → healing → serving`, continued access to the last verified data, and recorded repair history. Test a genuine document-access block separately.
- Use `testset/portals.yaml` for real portal tests. Aim for four countries, three portal software packages, at least five open tenders with public documents per positive case, and one or two access-blocked cases. Cover server-rendered, JavaScript, and multi-step sites, and hold back a few portals for final testing.
- Use `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked` locally. Run the contract checker and the complete demo against the built container as they become available. Verify the `linux/amd64` build early. Record test results and remaining gaps in the PR.
