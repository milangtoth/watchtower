# Live procurement portal test set

Use the development entries in portals.yaml first. Reserve the two holdout entries for a final run on a fresh Watchtower instance. For each case, post only its listing URL to POST /portals and poll GET /portals/{id}. Keep the country files outside the agent's inputs.

The five country files contain reference samples observed on 20 November 2026. Each row identifies a tender and one procurement document downloaded anonymously. Match the tender by source URL or portal identifier, compare the recorded fields, then fetch the document from Watchtower and compare its byte length and SHA-256. On TenderNed the ID identifies a publication and referenceNumber identifies the procurement.

Five is the sample size. Watchtower should return every open tender and its documents. Additional tenders and files are expected. A missing field in a reference row is unscored. For later live runs, check the source for expiry, amendments or withdrawal before treating a difference as a regression. For the UK case, check for a failed state and a reason explaining the registration requirement. Space requests to each portal at least one second apart.
