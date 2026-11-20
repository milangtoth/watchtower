// Compares a portal's published tenders with expected.json and downloads every file to check size and SHA-256.
// Usage: node testset/fixture-portal/check.mjs <watchtower-base-url> <portal-id> [output-dir]
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";

const [base, portalId, outArg] = process.argv.slice(2);
if (!base || !portalId) {
  console.error("Usage: node check.mjs <watchtower-base-url> <portal-id> [output-dir]");
  process.exit(2);
}
const out = resolve(outArg ?? join(dirname(fileURLToPath(import.meta.url)), "out"));
const expected = JSON.parse(readFileSync(join(out, "expected.json"), "utf8"));

const get = async path => {
  const response = await fetch(new URL(path, base));
  if (!response.ok) throw new Error(`GET ${path} returned HTTP ${response.status}`);
  return response;
};

const portal = await (await get(`/portals/${encodeURIComponent(portalId)}`)).json();
console.log(`Portal ${portal.state}: ${portal.reason} (${portal.tenderCount} tenders)`);
const published = await (await get(`/portals/${encodeURIComponent(portalId)}/tenders`)).json();
const byId = new Map(published.map(t => [t.id, t]));

const failures = [];
const fail = (id, field, want, got) => failures.push({ id, field, want, got });

for (const want of expected) {
  const got = byId.get(want.id);
  if (!got) {
    fail(want.id, "tender", "published", "missing");
    continue;
  }
  for (const field of ["title", "buyerName", "publicationDate", "submissionDeadline", "status", "description", "procedureType", "contractNature", "cpvCodes", "placeOfPerformance"]) {
    // The appendix requires null for a required field the portal does not show.
    const value = got[field] === undefined ? null : got[field];
    if (!isDeepStrictEqual(value, want[field])) fail(want.id, field, want[field], value);
  }
  const names = (got.documents ?? []).map(d => d.fileName).sort();
  if (!isDeepStrictEqual(names, [...want.documents].sort())) fail(want.id, "documents", want.documents, names);

  for (const doc of got.documents ?? []) {
    const bytes = Buffer.from(await (await get(doc.path)).arrayBuffer());
    const sha = createHash("sha256").update(bytes).digest("hex");
    if (bytes.length !== doc.sizeBytes) fail(want.id, `${doc.fileName} size`, doc.sizeBytes, bytes.length);
    if (sha !== doc.sha256) fail(want.id, `${doc.fileName} sha256`, doc.sha256, sha);
  }
}
for (const extra of published.filter(t => !expected.some(e => e.id === t.id))) fail(extra.id, "tender", "absent", "published");

if (failures.length) {
  for (const f of failures) console.log(`FAIL ${f.id} ${f.field}: expected ${JSON.stringify(f.want)}, got ${JSON.stringify(f.got)}`);
  console.log(`${failures.length} mismatches across ${new Set(failures.map(f => f.id)).size} of ${expected.length} tenders.`);
  process.exit(1);
}
console.log(`All ${expected.length} tenders and their files match the fixture.`);
