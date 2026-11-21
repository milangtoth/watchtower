# Tender Atlas


Run `docker compose up --build` with the root `.env` file to start both
services. Open http://localhost:43871 and choose **Portals**. Atlas forwards
registration, refresh, tender, run and document requests to the Rust service.
The **Auto-refresh portals** preference is stored in Watchtower. It starts on,
because scheduled reads are how Watchtower notices and repairs a broken portal
without a person. Turning it off stops scheduled reads and retries until it is
turned on again. Registration, refresh and repairs can use model credits. The dashboard on port
8080 remains read-only. Atlas Explorer uses its separate local publisher library.

The Python reference implementation under `app/` also supports direct XML/RSS,
optional Jev link ranking (`TYPESAFE_API_KEY`), and a configurable output
timezone (`WATCHTOWER_OUTPUT_TIMEZONE`). For this implementation, run
`python -m pytest tests` and start the worker with
`python -m uvicorn app.main:app --env-file .env --port 8080`.
