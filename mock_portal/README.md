# Switchable mock portal

Run `cargo run --locked --example mock_portal` from the repository root. The local fixture listens on `127.0.0.1:8091` by default. `MOCK_PORTAL_ADDR` overrides the bind address.

Register `http://127.0.0.1:8091/` with a native Watchtower process. From Docker Desktop, use `http://host.docker.internal:8091/`.

Wait for the first verified snapshot, then change the live listing and its detail templates:

```sh
curl -fsS -X POST http://127.0.0.1:8091/__fixture/change-layout
```

The updated layout uses a table and new detail links. The two tender identifiers stay stable, the first title changes, and both tenders link to revised files. Refresh the portal once, then inspect its run history and tender snapshots. The fixture continues to serve the old document URLs so Watchtower can verify that the previous published data stays readable until repair succeeds. Restart the mock process to return to its original layout.
