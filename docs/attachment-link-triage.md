# Watchtower attachment-link failure and test guide

## Summary

Watchtower can extract tender records from the GOV.UK Legal Aid Agency listing and fetch many of their documents. The live attempts on 2026-11-20 still published zero tenders. In each one, the reader reached an external Duty Solicitors application two hops away from the tender page and treated it as a required attachment. That application returned HTML with no file links, so the whole read failed. The initial learn and both repair attempts failed the same way.

The tender page never links to Duty Solicitors. The path was:

1. **Crime Contract 2025 Tender** lists the HTML attachment *Supervisor Forms and Guidance Crime Contract 2025* among its PDFs. The recipe correctly selects it as a document.
2. That HTML page has a single onward link, *Supervisor Forms and Guidance*, to a **different publication**, [Standard Crime Contract 2025](https://www.gov.uk/government/publications/standard-crime-contract-2025). The HTML fallback followed it because it was the only same-origin link labelled "forms" or "guidance".
3. The reader applied the recipe's `documents` selector to that page. The selector was learned for tender pages, and it matched all of the page's GOV.UK attachments, including **Police Station Register**. GOV.UK presents that item as an external attachment that points to `https://www.dutysolicitors.org/ords/f?p=109:45`, an Oracle APEX application.
4. The application returned HTML without file links, and `No attachment links matched` failed the read for all seven tenders.

Step 2 was not visible in the logs because only document-link fetches were logged. Repair could not fix the failure, because the model only saw the final host and path and its tender-page selector was already correct.

The fix tracks how each attachment link was found. Pages reached through the HTML fallback are read with the fallback alone (download attributes and file extensions), not with recipe selectors. On the Standard Crime Contract page, that yields its PDF and DOCX files and skips the extensionless application link. A link the recipe selects that turns out to be an empty dead end still fails the read, so a partially collected tender set cannot appear verified.

## What has been observed

| Evidence | What it establishes |
|---|---|
| The listing initially failed to attach because its first tender sat beyond the original 20,000-character raw HTML sample. | The original discovery problem was in sampling. The reader now samples the main content. |
| The first diagnostic run reported `Document link returned a web page`. A subsequent run logged the exact Supervisor Forms and Guidance path. | A link displayed alongside files can return an intermediate HTML page. |
| With the mixed attachment reader deployed, the trace logged the Supervisor Forms and Guidance path and then proceeded to fetch a separate Duty Solicitors URL. | The earlier intermediate-page blocker was passed on the live run. The local fixture separately proves the HTML-to-file path by checking returned bytes. |
| The subsequent failure was `Repair did not restore delivery: No attachment links matched on document page www.dutysolicitors.org/ords/f`. | Attachment discovery returned no matching links on that page under the current rules. The log records the final path without its query string. |
| The portal finished in `failed` with `tenderCount: 0` and `lastSuccessAt: null`. | No candidate from this portal was published. Staged blobs from unsuccessful reads are independent of the published API snapshot. |
| The container logs show the Supervisor page and then `www.dutysolicitors.org/ords/f` about 32 seconds later, at 16:00, 16:02, and 16:05 UTC. | The same path failed in the initial learn and in both repairs. The 32 seconds match the downloads queued ahead of it at one request per second. |
| The six current tender pages contain no `dutysolicitors.org` link. The Standard Crime Contract 2025 page lists it as the *Police Station Register* attachment, with query `?p=109:45`. | The link was reached through the HTML fallback hop described in the summary. The recipe did not select it on a tender page. |

The primary input is the [Legal Aid Agency tender listing](https://www.gov.uk/government/collections/tenders-for-legal-aid-contracts). It showed seven links in its **Current tenders** section in the captured test page. The **Closed tenders** section is separate. The current list may change, so count and compare URLs against the source at the time of each live test.

The [Crime Contract 2025 Tender](https://www.gov.uk/government/publications/crime-contract-2025-tender#documents) page lists PDFs alongside [Supervisor Forms and Guidance](https://www.gov.uk/government/publications/crime-contract-2025-tender/supervisor-forms-and-guidance-crime-contract-2025). GOV.UK labels the latter as HTML. Its only onward link goes to the separate [Standard Crime Contract 2025](https://www.gov.uk/government/publications/standard-crime-contract-2025) publication, which holds the supervisor declaration forms alongside the full contract set. This is a legitimate intermediate page to follow. Following it also attaches the whole Standard Crime Contract document set (about 16 files) to this tender. Decide separately whether that scope is wanted.

The Duty Solicitors link is the Standard Crime Contract page's *Police Station Register* attachment. GOV.UK marks it as external (`gem-c-attachment__thumbnail-image--external`) and shows the destination URL as metadata. The destination is an Oracle APEX application (`/ords/f?p=109:45`), not a document index. The earlier generic transport error was a distinct symptom and did not determine the terminal result of this run.

## Where the failure occurs

1. The worker fetches the listing, extracts tender entries, and visits the detail pages selected by the recipe. It reads HTML on the server. The app supplies its own API response after it has collected and checked a candidate.
2. The recipe can select direct file links with `direct_documents` and attachment pages with `document_page` on the same tender. With `document_page` set, `documents` selects files on those pages and `nested_document_pages` selects onward attachment pages. Without `document_page`, `documents` retains its direct-file behavior. See [recipe.rs](../src/recipe.rs).
3. The fetcher inspects the response. A file response is streamed, hashed, and staged. ZIP entries are unpacked into individual documents. An HTML response is checked for access blocks and then passed back to the reader for link extraction. See [fetch.rs](../src/fetch.rs).
4. Each queued attachment link records whether the recipe or the HTML fallback found it, and the page that linked to it. Recipe selectors apply to `document_page` and `nested_document_pages` targets. They also apply to HTML returned by a recipe-selected file link, but only when the recipe declares `document_page`. Without `document_page`, `documents` describes the tender row or detail page. Pages reached through the fallback use the fallback only. The HTML fallback checks the main content for download attributes and recognizable file extensions. In the absence of file links, it can follow one unambiguous same-origin link labelled as forms, documents, attachments, or guidance. Relative links resolve against the effective URL. Traversal deduplicates observed URLs and is limited to 32 attachment pages per tender and three levels of nesting. The worker logs `Inspecting attachment page` with the host, path, and link source for each attachment page it reads.
5. If a page has no usable links, the read fails as `Incomplete` and no candidate is produced. One dead end in one tender discards the whole read. The reason includes the host and path, and the page that linked to it, without query strings. The worker can start repair with that reason as evidence. When repair is exhausted, it reports `failed`. A successful candidate must pass verification before the worker updates the tender snapshot and file references. See [worker.rs](../src/worker.rs) and [model.rs](../src/model.rs).

A useful mental model is that `Content-Type: text/html` tells us how to read the response. It does not tell us whether the page is a tender attachment index, an external application, a genuine HTML document, or an access gate. The original failure came from applying tender-page selectors to a page that a heuristic had reached, then treating every match as a required attachment.

## Fast checks with the current code

Run from the repository root with the stable Rust toolchain. The integration suite binds a local fixture to loopback and uses a fake model.

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --locked --test service portal_lifecycle_preserves_verified_data_and_serves_document_bytes -- --nocapture
cargo test --locked --test service fallback_pages_skip_external_tools_but_recipe_dead_ends_still_fail -- --nocapture
```

Without a local Rust toolchain, run the same commands in the Dockerfile's build image:

```sh
docker run --rm -v "$PWD":/src:ro -v watchtower-cargo-registry:/usr/local/cargo/registry \
  -v watchtower-cargo-target:/target -e CARGO_TARGET_DIR=/target -w /src rust:1.100-slim-bookworm \
  sh -c 'rustup component add rustfmt clippy && cargo fmt --check &&
    cargo clippy --locked --all-targets -- -D warnings && cargo test --locked'
```

The last test models the GOV.UK chain with a second loopback origin standing in for the external application. It checks that the fallback hop delivers the PDFs without requesting the application. It also checks that a recipe-selected link to the same application still fails with its referring page in the reason.

The lifecycle test is the smallest existing end-to-end check for this part of the pipeline. The fixture in [tests/service.rs](../tests/service.rs) exercises a direct ZIP, a declared attachment page with a nested page, and a URL selected as a download that actually returns an HTML bridge. The API assertions compare the file bytes and hashes. The fixture also contains a repeated link, a link back to a visited page, and an access-blocked refresh. The fake model makes this repeatable and isolates the runner. This test does not exercise Luna's choice of selectors.

## Live reproduction using the same portal

Use the running Watchtower service. These commands require `curl` and `jq`. Use `http://127.0.0.1:8080` for the current local instance. For an isolated comparison, run a second container on another loopback port with its own volume and set `WATCHTOWER_URL` accordingly. Match the image version and model across comparisons. The live test requires `OPENAI_API_KEY` and `WATCHTOWER_MODEL=gpt-6-luna`. See the [README](../README.md) for Docker setup. When using plain `docker run --env-file .env`, use unquoted values or load the quoted values into the process environment before passing them by variable name.

### Start exactly one attempt

Registering the same URL returns its existing ID. A newly registered portal already has its first read queued. For the existing failed portal, explicitly refresh once and record the returned `runId`. Rebuild and restart the container first so the run uses the fix.

The active recipe is stored only when a read is published. Because this portal has never served, a refresh starts a fresh model `learn` rather than reusing a recipe, and a failure can add two repairs. Each attempt makes several model calls, so check the recorded token usage before repeating it.

```sh
base="${WATCHTOWER_URL:-http://127.0.0.1:8080}"
source_url='https://www.gov.uk/government/collections/tenders-for-legal-aid-contracts'
portal_id="$(curl -fsS "$base/portals" |
  jq -r --arg url "$source_url" '.[] | select(.url == $url) | .id')"

if [ -n "$portal_id" ]; then
  refresh="$(curl -fsS -X POST "$base/portals/$portal_id/refresh")"
  run_id="$(printf '%s\n' "$refresh" | jq -er .runId)"
  printf 'Refreshed portal %s, read %s\n' "$portal_id" "$run_id"
else
  portal_id="$(curl -fsS -X POST "$base/portals" \
    -H 'Content-Type: application/json' \
    --data "$(jq -nc --arg url "$source_url" '{url:$url}')" | jq -er .id)"
  printf 'Registered portal %s and queued its first read\n' "$portal_id"
fi
```

### Observe the run and classify the outcome

Run the following read-only checks in the same shell. Replace `watchtower-dev` if the container uses a different name. Use `docker logs -f` in a second terminal if you want to watch events as they happen.

```sh
curl -fsS "$base/portals/$portal_id" | jq '{state,reason,tenderCount,lastSuccessAt}'
curl -fsS "$base/portals/$portal_id/runs" | jq '.[-6:]'
docker logs --since 20m watchtower-dev 2>&1 |
  grep -E 'Inspecting HTML returned by document link|Inspecting attachment page|Recipe attempt failed|Portal fetch failed|Retrying portal read'
```

When retrying a failed portal, wait for the new read ID in the run list before interpreting an old `failed` state. Once that read finishes, keep checking while the portal is `healing` or a repair is active. The crawler spaces portal requests by at least one second and the full run can take several minutes. Repeated status GET requests are local. A new refresh can start additional external work once the current run finishes.

| Signal | Interpretation |
|---|---|
| `Inspecting HTML returned by document link` for the Supervisor Forms page, followed by later document work | The crawler advanced past the original HTML bridge. The log line by itself only establishes that inspection began. |
| `Inspecting attachment page` for `www.gov.uk/government/publications/standard-crime-contract-2025` with `source="fallback"` | The fallback followed the Supervisor page's onward link. With the fix, no `www.dutysolicitors.org` request should follow. |
| Portal `reason` or run result `No attachment links matched on document page …, linked from …` | A link reached a page with no usable links. This appears in the API, not the container logs. The `linked from` page tells you where to look. A recipe-selected dead end still fails by design. |
| `Portal fetch failed on … (document headers): …` | This is a transport failure while requesting a resource. Check the matching log entry for host, path, and cause. |
| Verifier rejects coverage, dates, or missing documents | The reader produced a candidate. Review the specific verification evidence before changing attachment navigation. The verifier receives sampled records and a separate total parsed count. |
| `Recipe attempt failed` with `stage`, `failure`, and `recipe` | One per failed read or verification, for the initial attempt and each repair, keyed by `run_id`. `recipe` is the full recipe JSON that was tried. Active recipes are stored only on publish, so this line is the only record of what a failed learn or repair produced. Compare its selectors against the detail page named in the failure. |
| `serving` with a new `lastSuccessAt` | The candidate was published. Compare its source URLs and files before marking the live test successful. |

### Verify the published bytes

After `serving`, compare tender URLs against the current source section and check the document names against at least one relevant tender detail page. The snippet below checks the SHA-256 and length of a document delivered through Watchtower's own API.

```sh
tenders="$(curl -fsS "$base/portals/$portal_id/tenders")"
printf '%s\n' "$tenders" | jq '{
  tenders: length,
  open: (map(select(.status == "open")) | length),
  documents: (map(.documents | length) | add // 0)
}'
doc="$(printf '%s\n' "$tenders" | jq -ce '[.[].documents[]] | first // error("no published documents")')"
file_path="$(printf '%s\n' "$doc" | jq -er .path)"
expected_hash="$(printf '%s\n' "$doc" | jq -er .sha256)"
expected_size="$(printf '%s\n' "$doc" | jq -er .sizeBytes)"
file="$(mktemp)"
curl -fsS "$base$file_path" -o "$file"
actual_hash="$(shasum -a 256 "$file" | awk '{print $1}')"
actual_size="$(wc -c < "$file" | tr -d '[:space:]')"
test "$actual_hash" = "$expected_hash" && test "$actual_size" = "$expected_size" &&
  printf 'Document verified (%s bytes, SHA-256 %s)\n' "$actual_size" "$actual_hash"
```

On systems without `shasum`, use `sha256sum "$file"`. Also inspect `fileName` and test a specific document reached through the attachment bridge. A hash match proves that Watchtower serves the bytes it recorded. Source comparison determines whether those bytes belong to the intended tender.

## Regression cases for the eventual fix

Add deterministic cases to the local fixture before leaning on repeated live Luna runs. Model nondeterminism and public-site changes make the live test a poor substitute for this coverage. The first two rows are covered. The others are still open.

| Case | Fixture | Expected behavior |
|---|---|---|
| Direct file and legitimate HTML bridge | One tender has a ZIP and an HTML path with a clearly labelled onward link to a PDF. | Publish both sets of files once. The bridge itself contributes no HTML blob or fake document. Covered by `portal_lifecycle_preserves_verified_data_and_serves_document_bytes`. |
| Fallback hop to an attachment list with an external tool | A tender links a PDF and an HTML bridge. The bridge's single labelled link leads to another page whose attachment list, matched by the recipe selector, includes a cross-origin postcode application. | Publish the PDFs from both pages and never request the application. A recipe-selected link to the same application still fails, naming its referring page. Covered by `fallback_pages_skip_external_tools_but_recipe_dead_ends_still_fail`. |
| Ambiguous attachment page | An item explicitly presented as a document returns HTML with several unrelated links and no attachment evidence. | Report an incomplete candidate with useful context. A previous verified snapshot remains readable. |
| Broken required file | A real downloadable attachment returns a temporary network error or a 404. | Preserve failure reporting. A tool-filtering change must not turn a missing required file into a healthy result. |
| Access gate | A selected attachment page requires a login or CAPTCHA. | Report the access block, stop following that path, and preserve verified data during healing. |
| Deduplication and bounds | Use duplicate URLs, fragments, a redirect to a visited page, a loop, and a chain beyond the nesting limit. | Fetch each known target once where possible, prevent loops, and produce an explicit error when a traversal bound is exceeded. |
| Old stored recipe | Load a recipe with only the legacy `documents` selector. | Retain direct-download behavior. A healthy repeat read still uses no model calls. |
| Real HTML document | A link is explicitly published as a self-contained HTML document with no onward files. | Decide separately whether to preserve HTML documents as downloadable artifacts. Empty link extraction alone cannot classify this as a tool. |

## Ways to resolve the classification problem

The implemented fix is a narrow form of the third option below. Link provenance and the referring page now travel with each queued link, and fallback-found pages no longer use recipe selectors. The other options remain available if a recipe-selected link on a tender's own page leads to a tool.

- **Tighten recipe selection first.** Capture the parent attachment block, the selected link label, and the submitted and effective URLs during reproduction. Use that evidence to teach the learner and repair step to select file and attachment-page links from the actual documents section. Explicitly distinguish links to application systems, calculators, and search tools. This approach retains the current runner and is easiest when the same selectors can cover all detail templates.
- **Add a narrowly defined tool outcome if selectors alone remain brittle.** Use positive evidence that the selected URL is a tool. Examples include GOV.UK's external-attachment marker (`gem-c-attachment__thumbnail-image--external`), or a link labelled as a postcode lookup together with a response whose main content is a lookup form and lacks attachment evidence. Preserve the existing incomplete outcome for a document-labelled link that unexpectedly has no files. A hostname, an HTML MIME type, or a missing extension alone is weak evidence. Test positive and ambiguous cases side by side.
- **Carry the attachment role through collection if the runner needs more evidence.** Keep the source detail URL and anchor label with a selected link, along with whether the recipe classified it as a file or attachment page. That allows the failure reason and model repair feedback to explain what was selected without exposing query strings. It also lets verification distinguish a required attachment from a navigation link.

## Live result after the fix

Run `dd541419` (image `57662208c362`, `gpt-6-luna`, 2026-11-20 16:33 UTC) confirmed the attachment fix. Both the read and its repair logged the Standard Crime Contract page with `source="fallback"` and made no request to `www.dutysolicitors.org`. The portal still ended `failed` at verification. The verifier found no documents on *Introduction of Inquest Category of Law*. That tender is a `/guidance/` page whose six PDFs use `<h2 class="gem-c-attachment__title">`, while the publication pages use `<h3>`. The learned recipe probably matched only the publication layout. That run predates recipe logging, so the selector can't be confirmed.

Two changes followed. When recipe document selectors match nothing on an HTML detail page, the reader now uses the HTML fallback on that page, and treats what it finds as fallback links. The learner is also told that detail pages can use different templates. The test `detail_pages_with_another_template_use_the_html_fallback` covers this.

Run `ae580e1e` (image `f94e348b99d7`, 2026-11-20 16:45 UTC) published on its first attempt, with no repair: `serving`, 7 open tenders, `lastSuccessAt` 16:48:28 UTC. The tender URLs match the seven links in **Current tenders**. *Introduction of Inquest Category of Law* has its six PDFs. *Crime Contract 2025 Tender* has 37 documents, including the three supervisor declaration forms reached through the fallback hop, and no Duty Solicitors link. For three files the API's bytes match their recorded SHA-256 and size: *Crime_Supervisor_Declaration_Form_.docx* (`52c190b6…`), *Police_Station_Register_Arrangements_2025.pdf* (`39eadcb2…`), and *2024_Standard_Civil_Contract_Inquests_Application_Guide_V1.pdf* (`138e9a64…`). The last one also matches the GOV.UK asset byte for byte. In this run the learned selector matched both templates, so neither the detail-page fallback nor `Recipe attempt failed` appeared in the logs.

Remaining gaps: `buyerName` is null even though each GOV.UK page names the Legal Aid Agency, and `submissionDeadline` is null. The Crime tender also carries the whole Standard Crime Contract document set.

Consider the fix verified when the deterministic mixed fixture publishes the expected bytes, the ambiguous and broken-file cases still preserve correctness, and the live run can account for the Duty Solicitors link and deliver the real attachments for the verified open tenders. Record the image ID, model, run ID, terminal reason, and one verified file hash alongside the result.
