# Real-portal test set

[portals.yaml](portals.yaml) records public listings, verified attachments, observed access blocks, and four holdout portals reserved for the final run. Each entry has `split: development` or `split: holdout`. Where present, `readiness` records the evidence available for that entry; a positive case with `needs-five-download-baseline` still needs qualification.

Two sets of independently collected answers sit beside the inventory, the `*-baseline.json` files and the country sample files (`CZ.json`, `IE.json`, `MT.json`, `NL.json`, `PL.json`). Watchtower's live results are tracked separately in [live validation](../docs/live-validation.md).

## Run the test set

Use the development entries first. Reserve the holdout entries for a final run on a fresh Watchtower instance. For each case, post only its listing URL to `POST /portals` and poll `GET /portals/{id}`. Keep the baseline and country files outside the agent's inputs.

To reproduce the connection evaluation, install PyYAML and build the probe example with `cargo build --locked --example probe`. Pass the compiled binary path to `scripts/train_ranker.py` to regenerate coefficients from the development split. Pass the same binary path to `scripts/eval_decisions.py` for a per-portal report. The evaluator uses the embedded weights. Set `OPENAI_API_KEY` and `WATCHTOWER_MODEL` for live decisions or pass `--rank-only` to report the ranker alone.

Development portals marked `unsupported-post` have no usable GET listing in the inventory. The trainer treats their probed candidates as negative examples. The evaluation lists them as unsupported POST and scores access blocks separately.

The country files contain reference samples observed on 20 November 2026. Each row identifies a tender and one procurement document downloaded anonymously. Match the tender by source URL or portal identifier, compare the recorded fields, then fetch the document from Watchtower and compare its byte length and SHA-256. On TenderNed the ID identifies a publication and `referenceNumber` identifies the procurement.

Five is the sample size. Watchtower should return every open tender and its documents. Additional tenders and files are expected. A missing field in a reference row is unscored. For later live runs, check the source for expiry, amendments or withdrawal before treating a difference as a regression. For a blocked case, check for a failed state and a reason that names the login, registration or CAPTCHA requirement. Space requests to each portal at least one second apart.

## Qualify a positive case

1. Select a manageable public listing with at least five currently open tenders. Prefer a stable search URL if the portal offers one. Keep the listing URL as the actual test input.
2. For five tenders, record the source URL or identifier, buyer, submission deadline, and one real attachment that downloads in a fresh session without logging in. A notice summary alone is insufficient for this check.
3. Record the observed filename and SHA-256 of one file per tender. Keep the baseline outside Watchtower's model inputs. On a live run, compare tender coverage and document bytes against it. Use `scripts/check_contract.py` for the HTTP shape and download checks.
4. Record portal software, run ID, terminal status, missing fields, and any access blocks. Keep temporary errors distinct from a confirmed login or CAPTCHA requirement.

For the blocked case, verify the page gates the requested action. Watchtower should report a specific access reason. Leave the holdout URLs out of development runs until final testing.
