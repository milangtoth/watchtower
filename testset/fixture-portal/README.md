# Fixture portal

A small fictional procurement portal for repeatable scanner and assistant tests. All buyers, references and documents are invented.

- Two server-rendered listing pages with `rel="next"` pagination, one detail page per tender, and downloadable PDFs.
- Eight tenders across the Netherlands, Germany and Belgium, written in Dutch, German and French.
- IT tenders (CPV 72…) in several countries, including a German one closing on the 10th of next month, for cross-country and "closing next month" searches.
- Edge cases: a notice that states no buyer ("Niet vermeld", expected `buyerName: null`), a tender whose deadline has passed (expected `closed`), a tender without documents, non-English procedure names, and missing postal codes.

Dates are relative to the build day, so rebuild before each test run.

## Run

Requires Node and Docker. Run from the repository root while Watchtower runs as a container.

```sh
node testset/fixture-portal/build.mjs
docker network create watchtower-test
docker run -d --name fixture-portal --network watchtower-test -v "$PWD/testset/fixture-portal/out/site:/usr/share/nginx/html:ro" nginx:1.27-alpine
docker network connect watchtower-test <watchtower-container>
curl -sS -X POST http://localhost:8080/portals -H 'Content-Type: application/json' -d '{"url":"http://fixture-portal/aanbestedingen/"}'
```

Wait until `GET /portals/{id}` reports `serving`, then compare the published snapshot with the fixture:

```sh
node testset/fixture-portal/check.mjs http://localhost:8080 <portal-id>
```

The checker compares every appendix field it can derive from the source, downloads each file to verify size and SHA-256, and exits non-zero on any mismatch.

`expected.json` is written from the fixture's source data, not from Watchtower's output. Procedure and contract nature map from the notice's language to the appendix values: Openbaar, Offenes Verfahren and Procédure ouverte are `open`; Niet-openbaar is `restricted`; Verhandlungsverfahren is `negotiated`.
