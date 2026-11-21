const RESULT_LIMIT = 8;
const PORTAL_BATCH_SIZE = 6;
const DATE_PATTERN = /^\d{4}-\d{2}-\d{2}$/;
const CPV_PREFIX_LIMIT = 8;

function fold(value) {
  return value.normalize("NFKD").replace(/\p{M}/gu, "").toLowerCase();
}

// Many portals publish no CPV codes, and titles are in the portal's language. These stems,
// matched inside folded titles and descriptions, let a sector filter or a sector word in the
// query find such tenders. Keys are CPV prefixes; the longest known prefix wins.
const SECTOR_TERMS = {
  "72": ["software", "informatik", "informationstechn", "edv", "it-", "ikt", "ict", "digital", "daten", "server", "netzwerk", "website", "webseite", "internet", "online", "portal", "plattform", "cloud", "hosting", "lizenz", "rechenzentrum", "programmier", "anwendung", "e-government", "informacni", "pocitac", "informatique", "logiciel", "numerique", "computer", "hardware", "laptop", "tablet", "monitor", "drucker", "map"],
  "48": ["software", "lizenz", "anwendung", "app ", "logiciel", "licence", "license"],
  "302": ["computer", "hardware", "laptop", "notebook", "tablet", "monitor", "server", "drucker", "pc ", "pocitac", "ordinateur"],
  "85": ["gesundheit", "klinik", "krankenhaus", "medizin", "arzt", "aerzt", "patient", "pflegeheim", "krankenpflege", "altenpflege", "pflegedienst", "rettungsdienst", "labor", "impf", "therap", "hospital ", "health", "medical", "nemocnic", "zdravot", "lekar", "sante", "medical", "zorg", "ziekenhuis"],
  "33": ["medizin", "medical", "roentgen", "rontgen", "implant", "steril", "desinfek", "labor", "diagnost", "zdravotnick", "medic"],
  "45": ["bau", "sanierung", "neubau", "umbau", "rohbau", "tiefbau", "hochbau", "strasse", "straße", "bruecke", "brucke", "fenster", "dach", "fassade", "heizung", "sanitaer", "sanitar", "elektroinstall", "abbruch", "trockenbau", "maler", "estrich", "stavb", "rekonstrukc", "oprava", "travaux", "construction", "renovat", "bouw"],
  "71": ["planung", "architekt", "ingenieur", "objektplanung", "tragwerk", "gutachten", "vermessung", "projektovan", "architect", "engineering"],
  "90": ["reinigung", "abfall", "entsorgung", "winterdienst", "muell", "mull", "uklid", "odpad", "nettoyage", "cleaning", "waste", "schoonmaak"],
  "79": ["beratung", "consulting", "uebersetz", "ubersetz", "druck", "personal", "veranstaltung", "marketing", "studie", "evaluation", "kommunikation", "poradenstv", "conseil"],
  "80": ["schul", "bildung", "schulung", "weiterbildung", "kurs", "training", "seminar", "skoleni", "vzdelav", "formation", "education"],
  "60": ["transport", "befoerderung", "beforderung", "fahrdienst", "schuelerbefoerderung", "doprava", "transport"],
  "34": ["fahrzeug", "kfz", "pkw", "lkw", "omnibus", "linienbus", "vozidl", "vehicule", "vehicle"],
  "09": ["strom", "erdgas", "energie", "waerme", "warme", "fernwaerme", "kraftstoff", "elektrin", "energy", "energie"],
  "15": ["lebensmittel", "verpflegung", "catering", "mittagessen", "speise", "potravin", "stravovan", "food"],
  "39": ["moebel", "mobel", "mobiliar", "ausstattung", "nabytek", "mobilier", "furniture"],
  "50": ["wartung", "instandhaltung", "instandsetzung", "reparatur", "servis", "udrzb", "maintenance", "repair"],
  "77": ["gruenpflege", "grunpflege", "garten", "baum", "landschaft", "spielplatz", "zelen", "espaces verts"],
  "66": ["versicherung", "bank", "finanz", "kredit", "pojisten", "assurance", "insurance"],
  "64": ["telekommunikation", "mobilfunk", "telefon", "postdienst", "telecom"],
  "35": ["sicherheit", "bewachung", "wachschutz", "feuerwehr", "polizei", "security"],
  "79710": ["bewachung", "wachschutz", "sicherheitsdienst", "security"],
};

// Sector words a person or the voice agent may type as a query, mapped to CPV prefixes.
const SECTOR_ALIASES = {
  it: ["72", "48", "302"], ict: ["72", "48", "302"], ikt: ["72", "48", "302"], software: ["72", "48"],
  digital: ["72", "48"], computer: ["302", "72"], hardware: ["302"], edv: ["72", "302"],
  health: ["85", "33"], healthcare: ["85", "33"], medical: ["85", "33"], medicine: ["85", "33"],
  gesundheit: ["85", "33"], hospital: ["85", "33"], care: ["85"],
  construction: ["45", "71"], building: ["45"], bau: ["45"], works: ["45"], infrastructure: ["45"],
  engineering: ["71"], architecture: ["71"], planning: ["71"],
  cleaning: ["90"], waste: ["90"], environment: ["90", "77"],
  consulting: ["79"], services: ["79"], education: ["80"], training: ["80"], schools: ["80"],
  transport: ["60", "34"], vehicles: ["34"], energy: ["09"], food: ["15"], catering: ["15"],
  furniture: ["39"], maintenance: ["50"], repair: ["50"], landscaping: ["77"], green: ["77"],
  insurance: ["66"], finance: ["66"], telecom: ["64"], security: ["79710", "35"],
};

function sectorTerms(prefixes) {
  const terms = new Set();
  for (const prefix of prefixes) {
    for (let length = prefix.length; length >= 2; length--) {
      const known = SECTOR_TERMS[prefix.slice(0, length)];
      if (known) {
        known.forEach(term => terms.add(term));
        break;
      }
    }
  }
  return [...terms];
}

function mentionsSector(searchable, prefixes) {
  return sectorTerms(prefixes).some(term => searchable.includes(term));
}

function tenderText(tender) {
  return fold([tender.title, tender.description, tender.buyerName, tender.referenceNumber].join(" ")) + " ";
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

function inSector(tender, prefixes, searchable) {
  if (tender.cpvCodes?.length) return tender.cpvCodes.some(code => prefixes.some(prefix => code.startsWith(prefix)));
  // Titles only: descriptions repeat portal boilerplate such as "Vergabeportal" or "online".
  return mentionsSector(fold(tender.title ?? "") + " ", prefixes);
}

function wordMatches(tender, word, searchable) {
  const sector = SECTOR_ALIASES[word];
  // A sector word matches by meaning or as a whole word, so "it" does not match "arbeiten".
  if (sector) return inSector(tender, sector, searchable) || searchable.split(/[^\p{L}\p{N}]+/u).includes(word);
  return searchable.includes(word);
}

/** mode "all" needs every query word; "any" needs one, as a fallback when "all" finds nothing. */
function matches(tender, filters, words, mode = "all") {
  if (tender.status !== "open") return false;
  if (filters.country && tender.placeOfPerformance?.countryCode !== filters.country) return false;
  const searchable = tenderText(tender);
  if (filters.cpv.length && !inSector(tender, filters.cpv, searchable)) return false;

  const date = tender.submissionDeadline?.slice(0, 10);
  if (filters.deadlineFrom && (!date || date < filters.deadlineFrom)) return false;
  if (filters.deadlineTo && (!date || date > filters.deadlineTo)) return false;

  if (!words.length) return true;
  return mode === "any"
    ? words.some(word => wordMatches(tender, word, searchable))
    : words.every(word => wordMatches(tender, word, searchable));
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
  const all = [];
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
        all.push({ ...tender, portal });
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

  // Nothing matched every word: fall back to tenders that match any of them, best first.
  let relaxed = false;
  if (!results.length && words.length > 1) {
    const scored = all
      .filter(tender => matches(tender, filters, words, "any"))
      .map(tender => {
        const searchable = tenderText(tender);
        return { tender, score: words.filter(word => wordMatches(tender, word, searchable)).length };
      });
    scored.sort((a, b) => b.score - a.score || deadline(a.tender) - deadline(b.tender));
    results.push(...scored.map(entry => entry.tender));
    relaxed = results.length > 0;
  } else {
    results.sort((a, b) => deadline(a) - deadline(b) || (a.title ?? "").localeCompare(b.title ?? ""));
  }
  return {
    filters,
    relaxed,
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
