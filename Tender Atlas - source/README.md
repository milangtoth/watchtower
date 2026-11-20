# Tender Atlas

Tender Atlas is a local web app for searching European public procurement records. Keep **starter.sqlite.gz** beside **Tender Atlas.exe**, then open the executable on Windows. It opens in your browser and creates an **Atlas library** folder beside the executable. Keep that folder to retain imported data and collection progress. Use **Quit app** in the footer to stop the local server.

## Use the index

- Search titles, descriptions, buyers, suppliers, procedure identifiers and CPV codes as indexed by each connector. Search uses word prefixes and the publisher’s language.
- Filter by country, notice type, source and record date. The date controls also offer a filter for tender notices with upcoming deadlines.
- Open a row to read the details, inspect the source payload and follow its official link. TED records also link to the complete published XML.
- Export all matching results to CSV or JSON Lines. JSON Lines includes each stored source payload and its origin. CSV preserves the original currency and protects spreadsheet formula cells.
- In **Collect notices**, select publishers and an inclusive date range. The app reads successive pages or archive files and stores the results in SQLite. Date windows are checkpointed separately for each source. Resume repeats any interrupted window. A partial import stays searchable.
- Data sources shows the date span and number of records on this computer. A span can include gaps. A notice published in several registers can have several records. Older and later notices from one procurement can also appear together. Stage labels describe the notice, and tender notices can have expired deadlines.
- Collection runs while the local app is running. Search and export also work offline. Select a date range again to refresh changes. Large backfills take more disk space, and bulk archives are cached under Atlas library/downloads.

All collections and portal previews share a limit of one publisher request per second, including redirects and retries. When Atlas detects a CAPTCHA it stops reading that publisher and remembers the exclusion in this library.

## Discover a portal

Open **Discover a portal** and paste a public homepage, notice listing, RSS or Atom feed, JSON endpoint, or XML sitemap. Atlas previews up to five records with source links. It looks for repeated records, follows promising links, and can read sitemap indexes and compressed sitemaps. For JavaScript listings the optional browser reader observes public, same-host GET requests. Windows builds use installed Microsoft Edge for this step.

The preview reports comparisons against sampled detail pages and a second page when available. Sitemap previews read the linked notice records directly. Checks describe a sample; they do not certify the completeness of a portal. The last checked sample and reusable read instructions are saved in the Atlas library. An open preview refreshes every five minutes. **Add sample to register** makes those records searchable and exportable.

Atlas trains a small local logistic regression to prioritize observed links using multilingual procurement, listing, feed, notice-ID and support-page features. Its seed examples are a compact sample of actual located records and API destinations from the provenance dataset plus observed navigation links. A saved, checked recipe supplies a positive example; **Review candidate links** lets you correct a label or mark a negative. Training uses labels across portals, balances publisher/class groups, excludes hostnames as predictors, and runs locally on the next discovery. **Export discovery training examples** downloads those inputs as JSON Lines. The small starter model is experimental. It ranks candidates; field extraction and verification use explicit rules.

## Source provenance and training exports

Open **Source provenance** in a notice or export **JSON lines with source data**. Each JSON line includes a `provenance` object alongside the normalized fields and the publisher payload. CSV includes the same object as a JSON-encoded column.

The [compressed training export](training/notice-provenance.jsonl.gz) contains one JSON line per starter notice, with its source, publisher reference, title, buyer and provenance. All 126,128 rows have a source URL. Positions within saved archives were verified for 110,709 rows, and another 9,469 matched a later response at the same URL. The remaining 5,950 rows have URL-only evidence.

- `request.url` is the exact public data URL, including query parameters. The host, path and original encoded query are also exposed separately. `record_url` is the official notice or catalogue link shown to a reader.
- New collections record the HTTP method, the JSON body for a POST, the final response URL after redirects, the retrieval timestamp and the SHA-256 of the response bytes. TED scroll tokens are specific to a search session and can expire.
- `record_locator` identifies the entry in that response. JSON uses RFC 6901 pointers with zero-based indices. CSV uses one-based data-record numbers after the header, so a quoted cell containing a newline still counts as one record. Atom uses a namespace-qualified XPath. ZIP locators include the exact archive member name.
- `discovered_from` records the parent catalogue or pagination link when the connector followed one. A connector that starts at a configured endpoint has no observed site-navigation chain.
- Use `evidence` when selecting training examples. `captured` means the collector recorded the request and position. `verified_from_archive` means the saved publisher file matched the indexed record. `verified_by_refetch` means an identical payload was found in a later response at the stored origin. `legacy_url_only` means that the original URL and publisher reference are available, while the historical record position is unknown. Recollecting the date can add a fresh observation.
- For evaluating discovery on unseen sites, group training and evaluation examples by publisher host. Many records share one response or URL pattern, so a random row split can put the same pattern on both sides.

## Publishers

| Source | Coverage and record dates |
| --- | --- |
| [Tenders Electronic Daily](https://docs.ted.europa.eu/ODS/latest/reuse/search-api.html) | EU Publications Office. All TED scopes, using iteration mode for each publication day. Normalized fields and the returned search record are stored, including source document links. |
| [Bekanntmachungsservice](https://oeffentlichevergabe.de/ui/de/publicationPolicy) | German OCDS daily packages, including notice versions. File selection uses the publisher’s Berlin calendar day. German Open Data is provided under CC0. |
| [BOAMP](https://www.boamp.fr/pages/donnees-ouvertes-et-api/) | French Direction de l’information légale et administrative (DILA), dataset BOAMP. The source record includes the official filename and publication date, and each indexed notice retains the API download URL. |
| [PLACSP](https://contrataciondelsectorpublico.gob.es/wps/portal/DatosAbiertos) | Spanish Ministry of Finance hosted buyer profiles, excluding minor contracts. Atom updates for the current month and monthly ZIP archives for older periods. Versions are identified by contract folder and update timestamp. The retained XML contains the entire Atom entry. |
| [BZP](https://ezamowienia.gov.pl/pl/integracja/) | Polish Public Procurement Office national bulletin. Eight notice types cover tenders, results, performance, amendments, competitions and intentions. A publisher block or rate limit can interrupt a range. The app retains the records already imported and reports the error. |
| [TenderNed](https://www.tenderned.nl/cms/nl/aanbesteden-in-cijfers/datasets-aanbestedingen) | Dutch Ministry of Economic Affairs. The app discovers the published annual JSON releases and indexes the selected dates. Newly published notices become available as the government updates the archive. |
| [eTenders](https://data.gov.ie/dataset/contract-notices-published-on-etenders) | Irish Office of Government Procurement, CC BY 4.0. Historical competitions and awards. Record date uses the notice or creation date, then award or cancellation date. Rows with none of those dates are outside the date-range index. Distinct archive rows are retained by content because some share a tender ID. A changed row may represent a later snapshot. |
| [Find a Tender and Contracts Finder](https://www.gov.uk/government/publications/open-contracting) | UK Cabinet Office, Open Government Licence v3.0. Find a Tender uses the update-time window and follows the next-page link. Contracts Finder reads the complete daily flattened OCDS CSV and reconstructs each release. |

## Run the source

Python 3.12 or newer is required for the source version. From this folder install `python -m pip install -r requirements-discovery.txt` and run `python atlas.py`. The desktop build bundles the discovery dependencies and Mozilla trust roots. Optional command-line flags are `--port`, `--data` and `--no-browser`. The source and Windows executable share one starter index of 126,128 published records. The executable and the compressed index are stored with Git LFS. Install Git LFS and run `git lfs pull` after cloning to download both files.

The interface is served only on the loopback interface. The geographic illustration uses public-domain [Natural Earth](https://www.naturalearthdata.com/) boundaries. The bundled CA certificate file is distributed by certifi under the terms in CA-LICENSE.txt.


## Build the Windows app

From this folder on Windows, install PyInstaller with `python -m pip install pyinstaller==6.22.3`, then run `python -m PyInstaller --noconfirm "Tender Atlas.spec"`. The executable is written to `dist/Tender Atlas.exe`. Place `starter.sqlite.gz` alongside it to preload the index on another computer.

## Additional municipal sources

These portals were identified for possible future integration.

- [Barcelona municipal tenders](https://licitacions.bcn.cat/) and the [Catalan procurement dataset](https://analisi.transparenciacatalunya.cat/Economia/Contractaci-p-blica-a-Catalunya-publicacions-a-la-/ybgg-dgi6) cover local authorities and smaller projects.
- [Berlin’s procurement portal](https://www.berlin.de/vergabeplattform/veroeffentlichungen/bekanntmachungen/) offers an RSS feed for new notices.
- [Amsterdam’s procurement calendar](https://www.amsterdam.nl/ondernemen/inkoop-opdrachten/aanbesteding-voorbereiden-inschrijven/) lists contracts in preparation. The existing TenderNed import covers published notices.
- [Basque Government procurement APIs](https://opendata.euskadi.eus/comunidad-open-data/-/2024/nueva-api-rest-sobre-la-plataforma-kontratazioa/) cover regional and municipal buyers.
## Portal operator API

The container listens on port 8080 and stores its SQLite library in the mounted /data directory. It serves the dashboard at / and the JSON API from the same process.

- POST /portals with {"address":"https://example.gov"} registers a portal and performs its first refresh.
- GET /portals and GET /portals/{id} return portal delivery state.
- DELETE /portals/{id} removes its registration and associated run history.
- POST /portals/{id}/refresh reads the portal again.
- GET /portals/{id}/tenders returns imported sample notices.
- GET /portals/{id}/runs returns refresh and integration history.

Delivery states are integrating, serving, healing and failed. Tenders expose identifier, address, title, buyer, publication date, deadline, status and source payload. Portal discovery currently verifies and imports a sample of up to five notices, not a complete archive. Existing publisher collectors remain in the Atlas collection workflow.
