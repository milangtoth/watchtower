const RESULT_LIMIT = 8;
const PORTAL_BATCH_SIZE = 6;
const DATE_PATTERN = /^\d{4}-\d{2}-\d{2}$/;

function fold(value) {
  return value.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase();
}

function isDate(value) {
  if (!DATE_PATTERN.test(value)) return false;
  const date = new Date(value);
  return Number.isFinite(date.getTime()) && date.toISOString().slice(0, 10) === value;
}

function normalizeFilters(input) {
  const query = String(input.query ?? "").trim().slice(0, 120);
  const country = String(input.country ?? "").trim().toUpperCase();
  const cpv = String(input.cpv ?? "").trim();
  const deadlineFrom = String(input.deadline_from ?? "").trim();
  const deadlineTo = String(input.deadline_to ?? "").trim();

  if (country && !/^[A-Z]{2}$/.test(country)) {
    throw new Error("Use a two-letter country code, such as NL or DE.");
  }
  if (cpv && !/^\d{2,8}$/.test(cpv)) {
    throw new Error("Use a CPV code or prefix of two to eight digits.");
  }
  if ([deadlineFrom, deadlineTo].some(date => date && !isDate(date))) {
    throw new Error("Use dates in YYYY-MM-DD format.");
  }
  if (deadlineFrom && deadlineTo && deadlineFrom > deadlineTo) {
    throw new Error("The closing date range is reversed.");
  }

  return { query, country, cpv, deadlineFrom, deadlineTo };
}

async function readList(path, signal) {
  const response = await fetch(path, { signal: AbortSignal.any([signal, AbortSignal.timeout(8000)]) });
  if (!response.ok) throw new Error("Tender data is unavailable.");
  const list = await response.json().catch(() => null);
  if (!Array.isArray(list)) throw new Error("Tender data is unavailable.");
  return list;
}

function matches(tender, filters, words) {
  if (tender.status !== "open") return false;
  if (filters.country && tender.placeOfPerformance?.countryCode !== filters.country) return false;
  if (filters.cpv && !tender.cpvCodes?.some(code => code.startsWith(filters.cpv))) return false;

  const date = tender.submissionDeadline?.slice(0, 10);
  if (filters.deadlineFrom && (!date || date < filters.deadlineFrom)) return false;
  if (filters.deadlineTo && (!date || date > filters.deadlineTo)) return false;

  const searchable = fold([tender.title, tender.description, tender.buyerName, tender.referenceNumber].join(" "));
  return words.every(word => searchable.includes(word));
}

function deadline(tender) {
  const timestamp = Date.parse(tender.submissionDeadline);
  return Number.isFinite(timestamp) ? timestamp : Infinity;
}

export async function searchOpenTenders(input, signal) {
  const filters = normalizeFilters(input);
  const words = fold(filters.query).match(/[\p{L}\p{N}]+/gu) ?? [];
  const portals = await readList("/portals", signal);
  const countries = new Set();
  const results = [];
  let unavailablePortals = 0;

  for (let start = 0; start < portals.length; start += PORTAL_BATCH_SIZE) {
    signal.throwIfAborted();
    const lists = await Promise.allSettled(
      portals.slice(start, start + PORTAL_BATCH_SIZE).map(async portal => ({
        portal,
        tenders: await readList("/portals/" + encodeURIComponent(portal.id) + "/tenders", signal),
      })),
    );

    for (const entry of lists) {
      if (entry.status === "rejected") {
        unavailablePortals++;
        continue;
      }
      const { portal, tenders } = entry.value;
      for (const tender of tenders) {
        const country = tender.placeOfPerformance?.countryCode;
        if (country) countries.add(country);
        if (matches(tender, filters, words)) results.push({ ...tender, portal });
      }
    }
  }

  signal.throwIfAborted();
  if (portals.length && unavailablePortals === portals.length) {
    throw new Error("Tender data is unavailable from every connected portal.");
  }

  results.sort((a, b) => deadline(a) - deadline(b) || (a.title ?? "").localeCompare(b.title ?? ""));
  return {
    filters,
    results: results.slice(0, RESULT_LIMIT),
    total: results.length,
    countries: [...countries].sort(),
    searchedPortals: portals.length - unavailablePortals,
    unavailablePortals,
  };
}
