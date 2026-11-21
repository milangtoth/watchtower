# Demo video script (at most 3:00)

Record at 1280×720 with the dashboard at `http://localhost:8080/`. Wear headphones for the voice part. Start from a clean volume so the attach is visible, and keep the mock portal running (`cargo run --locked --example mock_portal`).

| Time | Show | Say |
|---|---|---|
| 0:00–0:15 | Dashboard overview: portals serving, tenders held. | "Watchtower learns to read public procurement portals and serves their tenders and documents through one API. This is the operator view." |
| 0:15–0:50 | **1. Attach.** In a terminal, `POST /portals` with a new portal URL. Back on the dashboard, the card appears as *integrating* and turns *serving*. Open **Runs and tenders** and click a document link. | "We give it only a URL. It probes the portal, picks a way to read it, learns a recipe, checks the result, and publishes. Documents download from our own API with matching hashes." |
| 0:50–1:20 | **2. Chosen method.** Open **Connection** on the new card: method badge, entry URL, rationale, ranked alternatives with scores and probe evidence. | "Here is how it decided. The chosen method, why the model picked it, and the ranked fallbacks with what each probe saw." |
| 1:20–2:10 | **3. Repair.** Run `python scripts/demo_repair.py --portal http://host.docker.internal:8091/`. Show the mock card go *serving → healing* (tenders still listed), then back to *serving*. Open **Runs and tenders**: failed read, then repair ok. | "Now the portal changes its layout. Still HTTP 200, but the old recipe breaks. Watchtower notices, keeps serving the last verified tenders while healing, learns a new recipe, verifies it, and switches over." |
| 2:10–2:50 | **4. Voice.** Open **Tender assistant** → **Start conversation**. Ask: "Why is TenderNed failing?", then "How do we read Find a Tender?", then "Show me open tenders in the IT sector." | "The assistant answers from the same data: portal state, the connection decision, and published tenders." |
| 2:50–3:00 | Back on the overview. | "One container, port 8080. Attaching and repairing this portal cost under eight cents of model use. Thanks." |

Before recording:
- Rerun the repair demo once against a fresh mock registration. `demo_repair.py` refuses a URL that is already registered.
- Use real connection decisions. Delete any fake ones seeded for development (model "fake fixture (not a model decision)").
- Check the cost line against the README cost table.
