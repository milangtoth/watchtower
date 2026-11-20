# Live validation

## GOV.UK attachment and metadata

The metadata refresh completed as read `a8aee8d8-273a-4b6b-b457-84b94ed1b1af` on 20 November 2026. All seven published URLs matched the source collection's **Current tenders** section. Four records remained open after the reader applied explicit deadlines. The snapshot had 104 document references.

- All seven records had the Legal Aid Agency as buyer and a publication date.
- The Inquests record had a submission deadline of `2027-03-31T17:00:00+01:00`, matching the explicit application deadline on the source. [Inquests guidance](https://www.gov.uk/guidance/introduction-of-inquest-category-of-law)
- The Crime Contract record had 24 document references, down from 37 in the earlier successful run. The reader uses the attachment bridge's label to narrow fallback downloads when the destination lists files that match that label.
- The API bytes for `2026_CDD_Contract_-_Amendment_Notice_26_August_2025.pdf` matched their recorded SHA-256 and 108,939-byte length.
- Three records still had no stored deadline. The fallback requires an unambiguous date with a time and known timezone. A prose cutoff that applies only to one assessment round can differ from the overall submission deadline.
- Healthy refresh `aef76105-1208-4b40-a622-ae90e60b7773` completed successfully. The saved model-usage rows and token totals were identical immediately before and after the refresh.

## Live Luna repair

The mock portal was registered at `http://host.docker.internal:8091/` on Docker Desktop. The original read `7ed95b56-7c35-40d7-bac3-f225de6b1b7a` published two tenders with downloadable files. After the mock switched from article cards to a table and changed its detail markup, read `41a46331-f77d-4c1e-8a22-235dacef9a5b` failed and Watchtower entered `healing`.

During healing, the tender snapshot and bytes of the original file matched the pre-change data. Repair `e2e2509c-6035-408b-8311-aa5fa12e3622` restored `serving`, retained both tender IDs, and published the amended Park lighting title and replacement files. The HTTP checker verified the repaired snapshot and the SHA-256 of `PARK-01-v2.txt`.

The [dashboard demo](watchtower-demo.mp4) replays the four API snapshots from this run. The benchmark and reproduction steps are in the [README](../README.md).

Healthy refresh `ed74d23d-c25c-4bdc-9d11-3b4fa93e92c4` used zero model calls, verified by the same SQLite ledger comparison. A second, fresh fixture registration passed the external HTTP check for registration, refresh deduplication, downloads, and deletion.

## Additional public portals

- eTendersNI reached `failed` with a CAPTCHA-specific reason and zero tenders. The official search form displays that gate. [eTendersNI search](https://etendersni.gov.uk/epps/prepareCurrentOpportunities.do?currentType=cft)
- The first DTVP attempt failed discovery on an invalid CSS selector. The retry, `888d7fd8-23ee-4871-88d6-164b63dcc632`, used all six discovery calls and found public tender and document pages. Its terminal reason was that the search endpoint sample was an empty template and the landing page only showed featured tenders, leaving no verified complete listing or pagination rule. Separately, one public document from each of five open tenders downloaded and was hashed. [DTVP source baseline](../testset/dtvp-baseline.json)
- Ireland eTenders has a server-rendered listing with submission deadlines and a public **Proceed without association** document flow. Five open notices each yielded a ZIP or PDF, verified outside Watchtower. [Ireland source baseline](../testset/etenders-ie-baseline.json)
- Catalonia PSCP and TenderNed both load notices via public, paginated JSON. Their source baselines each record five open notices and a hashed downloadable PDF. These listings expose page numbers and totals and build detail or document-index URLs from identifiers. The current recipe can follow explicit URLs. Watchtower attachment of these two sources is untested. [Catalonia source baseline](../testset/catalonia-pscp-baseline.json) · [TenderNed source baseline](../testset/tenderned-baseline.json)
- The initial HTML response for e-Zamówienia contained no tender anchors. The listing still needs a supported discovery path and an independently confirmed baseline.
- Two candidate listings are reserved for final testing. The [test inventory](../testset/portals.yaml) records which entries still need qualification.

## GOV.UK source inspection

On 20 November 2026 the collection and its seven **Current tenders** pages were fetched read-only, one request per 1.2 seconds, and compared with the snapshot Watchtower was serving. No model was called and the service was not changed. The independent answers are in the [GOV.UK source baseline](../testset/govuk-laa-baseline.json).

| Field | Watchtower snapshot | Source | Verdict |
|---|---|---|---|
| `id` | null on all 7 | Each page carries a stable `govuk:content-id` UUID in its metadata. No buyer reference number is shown. | Gap: a portal-owned identifier exists, but page samples given to the model cover only the main content, not the page head. |
| `buyerName` | Legal Aid Agency on all 7 | Legal Aid Agency | Match |
| `publicationDate` | 7 dates | Match `govuk:first-published-at` on all 7 | Match |
| `submissionDeadline` | 4 stored | All 4 match the stated date, time and UK offset | Match |
| Re-opening: 2024 Standard Civil Contract | open, no deadline | "must be submitted by 11:59 on 31 December 2025" | **Miss: an explicit, past deadline was not read, so the tender is wrongly open** |
| 2027 Peer Reviewer | open, no deadline | The stated cut-off (26 June 2026) was amended; the new deadline is only in a PDF | Correctly left missing |
| Crime Contract 2025 | open, no deadline | Stage 3 closes "on, or around, 30 September 2034", with no time | Correctly left missing |

The collection lists all seven under **Current tenders**, so the portal does not close them itself. Status therefore depends on the deadline. Of the seven, the correct statuses on this date are three open and four closed. Watchtower reports four open.
