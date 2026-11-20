# Real-portal test set

[portals.yaml](portals.yaml) records public listings, verified attachments, an observed CAPTCHA gate, and two candidates reserved for final testing. The readiness field records the evidence available for each entry. A positive case with `needs-five-download-baseline` still needs qualification.

As of 20 November 2026, the source baseline covers four countries and four software packages. DTVP, Ireland eTenders, Catalonia PSCP, and TenderNed each had five open tenders with a verified anonymous download. Watchtower's live results are tracked separately in [live validation](../docs/live-validation.md).

## Qualify a positive case

1. Select a manageable public listing with at least five currently open tenders. Prefer a stable search URL if the portal offers one. Keep the listing URL as the actual test input.
2. For five tenders, record the source URL or identifier, buyer, submission deadline, and one real attachment that downloads in a fresh session without logging in. A notice summary alone is insufficient for this check.
3. Record the observed filename and SHA-256 of one file per tender. Keep the baseline outside Watchtower's model inputs. On a live run, compare tender coverage and document bytes against it. Use `scripts/check_contract.py` for the HTTP shape and download checks.
4. Record portal software, run ID, terminal status, missing fields, and any access blocks. Keep temporary errors distinct from a confirmed login or CAPTCHA requirement.

For the blocked case, verify the page gates the requested action. Watchtower should report a specific access reason. Leave the holdout URLs out of development runs until final testing.
