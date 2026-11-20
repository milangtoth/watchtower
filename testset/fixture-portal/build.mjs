// Builds the fixture portal into a static site plus expected.json.
// Usage: node testset/fixture-portal/build.mjs [output-dir]   (default: testset/fixture-portal/out)
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { fixtureTenders, slug } from "./tenders.mjs";

const out = resolve(process.argv[2] ?? join(dirname(fileURLToPath(import.meta.url)), "out"));
const site = join(out, "site");
const tenders = fixtureTenders();
const PER_PAGE = 4;
const pages = Math.ceil(tenders.length / PER_PAGE);

rmSync(out, { recursive: true, force: true });
mkdirSync(join(site, "aanbestedingen", "documenten"), { recursive: true });

const esc = s => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const shown = iso => iso.replace("T", " ").replace(/:00([+-]\d\d:\d\d)$/, " ($1)");
const docUrl = (t, d) => `/aanbestedingen/documenten/${slug(t)}-${d}`;

const page = (title, body) => `<!doctype html>
<html lang="nl"><head><meta charset="utf-8"><title>${esc(title)} | Aanbestedingsportaal Lindenhaven</title></head>
<body><header><a href="/aanbestedingen/">Aanbestedingsportaal Lindenhaven</a></header>
<main>${body}</main><footer>Testportaal met fictieve gegevens.</footer></body></html>\n`;

function listing(items, pageNo) {
  const rows = items.map(t => `<tr class="tender">
  <td class="ref">${esc(t.ref)}</td>
  <td class="title"><a href="/aanbestedingen/${slug(t)}.html">${esc(t.title)}</a></td>
  <td class="buyer">${t.buyer ? esc(t.buyer) : "—"}</td>
  <td class="country">${t.country}</td>
  <td class="deadline"><time datetime="${t.deadline}">${shown(t.deadline)}</time></td>
</tr>`).join("\n");
  const prev = pageNo > 1 ? `<a rel="prev" href="/aanbestedingen/${pageNo === 2 ? "" : `pagina-${pageNo - 1}.html`}">Vorige</a>` : "";
  const next = pageNo < pages ? `<a rel="next" class="next" href="/aanbestedingen/pagina-${pageNo + 1}.html">Volgende</a>` : "";
  return page(`Aanbestedingen, pagina ${pageNo}`, `<h1>Aanbestedingen</h1>
<p class="count">${tenders.length} aanbestedingen gevonden. Pagina ${pageNo} van ${pages}.</p>
<table class="results"><thead><tr><th>Kenmerk</th><th>Titel</th><th>Opdrachtgever</th><th>Land</th><th>Sluitingsdatum</th></tr></thead>
<tbody>
${rows}
</tbody></table>
<nav class="pagination">${prev} ${next}</nav>`);
}

function detail(t) {
  const docs = t.docs.length
    ? `<ul class="documents">${t.docs.map(d => `<li><a href="${docUrl(t, d)}">${esc(d)}</a></li>`).join("")}</ul>`
    : `<p class="documents-none">Geen documenten gepubliceerd.</p>`;
  return page(t.title, `<article class="notice">
<h1>${esc(t.title)}</h1>
<dl>
<dt>Kenmerk</dt><dd class="ref">${esc(t.ref)}</dd>
<dt>Opdrachtgever</dt><dd class="buyer">${t.buyer ? esc(t.buyer) : "Niet vermeld"}</dd>
<dt>Publicatiedatum</dt><dd class="published"><time datetime="${t.published}">${t.published}</time></dd>
<dt>Sluitingsdatum</dt><dd class="deadline"><time datetime="${t.deadline}">${shown(t.deadline)}</time></dd>
<dt>Procedure</dt><dd class="procedure">${esc(t.procedure[0])}</dd>
<dt>Type opdracht</dt><dd class="nature">${esc(t.nature[0])}</dd>
<dt>CPV-code</dt><dd class="cpv">${t.cpv}</dd>
<dt>Plaats van uitvoering</dt><dd class="place"><span class="postal">${t.postal ?? ""}</span> <span class="city">${esc(t.city)}</span>, <span class="country">${t.country}</span></dd>
</dl>
<h2>Omschrijving</h2>
<p class="description">${esc(t.description)}</p>
<h2>Documenten</h2>
${docs}
</article>`);
}

/** A minimal valid one-page PDF. */
function pdf(text) {
  const content = `BT /F1 12 Tf 72 720 Td (${text.replace(/[()\\]/g, "")}) Tj ET`;
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>",
    `<< /Length ${content.length} >>\nstream\n${content}\nendstream`,
    "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
  ];
  let body = "%PDF-1.4\n";
  const offsets = [];
  objects.forEach((o, i) => { offsets.push(body.length); body += `${i + 1} 0 obj\n${o}\nendobj\n`; });
  const xref = body.length;
  body += `xref\n0 ${objects.length + 1}\n0000000000 65535 f \n` + offsets.map(o => String(o).padStart(10, "0") + " 00000 n \n").join("");
  return body + `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
}

for (let p = 1; p <= pages; p++) {
  writeFileSync(join(site, "aanbestedingen", p === 1 ? "index.html" : `pagina-${p}.html`), listing(tenders.slice((p - 1) * PER_PAGE, p * PER_PAGE), p));
}
for (const t of tenders) {
  writeFileSync(join(site, "aanbestedingen", `${slug(t)}.html`), detail(t));
  for (const d of t.docs) writeFileSync(join(site, docUrl(t, d)), pdf(`${t.ref} - ${d} - fictief testdocument`));
}
writeFileSync(join(site, "index.html"), page("Start", `<p><a href="/aanbestedingen/">Bekijk alle aanbestedingen</a></p>`));

// Expected values, written from the source data rather than from Watchtower's output.
const now = Date.now();
const expected = tenders.map(t => ({
  id: t.ref,
  title: t.title,
  buyerName: t.buyer,
  publicationDate: t.published,
  submissionDeadline: t.deadline,
  status: Date.parse(t.deadline) <= now ? "closed" : "open",
  description: t.description,
  procedureType: t.procedure[1],
  contractNature: t.nature[1],
  cpvCodes: [t.cpv],
  placeOfPerformance: { ...(t.postal ? { postalCode: t.postal } : {}), city: t.city, countryCode: t.country },
  documents: t.docs.map(d => `${slug(t)}-${d}`),
}));
writeFileSync(join(out, "expected.json"), JSON.stringify(expected, null, 2) + "\n");

const docCount = tenders.reduce((n, t) => n + t.docs.length, 0);
console.log(`${tenders.length} tenders on ${pages} listing pages with ${docCount} documents -> ${site}`);
