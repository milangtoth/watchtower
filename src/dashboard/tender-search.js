const RESULT_LIMIT = 8;
const PORTAL_BATCH_SIZE = 6;
const DATE_PATTERN = /^\d{4}-\d{2}-\d{2}$/;
const CPV_PREFIX_LIMIT = 8;

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
  // One prefix or several, so a sector such as IT can span CPV divisions 72, 48 and 302.
  const cpv = [...new Set(String(input.cpv ?? "").split(/[\s,;]+/).filter(Boolean))];
  const sector = String(input.sector ?? "").trim().slice(0, 60);
  const deadlineFrom = String(input.deadline_from ?? "").trim();
  const deadlineTo = String(input.deadline_to ?? "").trim();

  if (country && !/^[A-Z]{2}$/.test(country)) {
    throw new Error("Use a two-letter country code, such as NL or DE.");
  }
  if (cpv.some(prefix => !/^\d{2,8}$/.test(prefix))) {
    throw new Error("Use CPV codes or prefixes of two to eight digits, separated by commas.");
  }
  if (cpv.length > CPV_PREFIX_LIMIT) {
    throw new Error(`Use at most ${CPV_PREFIX_LIMIT} CPV prefixes.`);
  }
  if ([deadlineFrom, deadlineTo].some(date => date && !isDate(date))) {
    throw new Error("Use dates in YYYY-MM-DD format.");
  }
  if (deadlineFrom && deadlineTo && deadlineFrom > deadlineTo) {
    throw new Error("The closing date range is reversed.");
  }

  return { query, country, cpv, sector: cpv.length ? sector : "", deadlineFrom, deadlineTo };
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
  if (filters.cpv.length && !tender.cpvCodes?.some(code => filters.cpv.some(prefix => code.startsWith(prefix)))) return false;

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

function portalName(portal) {
  try {
    return new URL(portal.url).hostname;
  } catch {
    return portal.url ?? portal.id;
  }
}

/** A portal can only be searched once it has published a verified read. */
function portalSummary(portal, published) {
  const summary = { name: portalName(portal), state: portal.state, published };
  if (portal.state !== "serving" && portal.reason) summary.reason = String(portal.reason).slice(0, 200);
  return summary;
}

/** Portals identify tenders by source ID, falling back to the notice URL when the source has none. */
export function tenderKey(tender) {
  return tender.id ? "id:" + tender.id : tender.url ? "url:" + tender.url : null;
}

async function readPortal(portalId, signal) {
  const response = await fetch("/portals/" + encodeURIComponent(portalId), { signal: AbortSignal.any([signal, AbortSignal.timeout(8000)]) });
  if (response.status === 404) throw new Error("That portal is no longer connected.");
  if (!response.ok) throw new Error("Portal status is unavailable.");
  return response.json();
}

/** Reads the current published record for one tender, so answers reflect the latest verified snapshot. */
export async function findTender(portalId, key, signal) {
  const [portal, tenders] = await Promise.all([
    readPortal(portalId, signal),
    readList("/portals/" + encodeURIComponent(portalId) + "/tenders", signal),
  ]);
  const tender = tenders.find(item => tenderKey(item) === key);
  if (!tender) throw new Error("That tender is no longer in the portal's published data.");
  return { tender, portal: portalSummary(portal, Boolean(portal.lastSuccessAt)), lastSuccessAt: portal.lastSuccessAt };
}

export async function searchOpenTenders(input, signal) {
  const filters = normalizeFilters(input);
  const words = fold(filters.query).match(/[\p{L}\p{N}]+/gu) ?? [];
  const connected = await readList("/portals", signal);
  const portals = connected.filter(portal => portal.lastSuccessAt);
  const portalsWithoutData = connected.filter(portal => !portal.lastSuccessAt).map(portal => portalSummary(portal, false));
  const countries = new Set();
  const results = [];
  let unavailablePortals = 0;
  // Open tenders a CPV filter cannot judge, because their portal publishes no CPV codes for them.
  let withoutCpv = 0;
  const portalsWithoutCpv = new Set();

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
        else if (filters.cpv.length && !tender.cpvCodes?.length
          && matches(tender, { ...filters, cpv: [] }, words)) {
          withoutCpv++;
          portalsWithoutCpv.add(portalName(portal));
        }
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
    portals: [...portals.map(portal => portalSummary(portal, true)), ...portalsWithoutData],
    portalsWithoutData: portalsWithoutData.length,
    tendersWithoutCpv: withoutCpv,
    portalsWithoutCpv: [...portalsWithoutCpv].sort(),
  };
}
