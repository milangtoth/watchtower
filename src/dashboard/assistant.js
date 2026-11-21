import { findTender, searchOpenTenders, tenderKey } from "./tender-search.js";

const assistantDialog = document.querySelector('#assistant-dialog');
const voicePanel = document.querySelector(".voice-panel");
const voiceState = document.querySelector("#voice-state");
const voiceError = document.querySelector("#voice-error");
const voiceHint = document.querySelector("#voice-hint");
const startCallButton = document.querySelector("#start-call");
const endCallButton = document.querySelector("#end-call");
const muteCallButton = document.querySelector("#mute-call");
const callActions = document.querySelector("#call-actions");
const transcript = document.querySelector("#transcript");
const transcriptEmpty = document.querySelector("#transcript-empty");
const searchForm = document.querySelector("#tender-search");
const tenderResults = document.querySelector("#tender-results");
const searchStatus = document.querySelector("#search-status");
const resultsEmpty = document.querySelector("#results-empty");
const resultsMessage = document.querySelector("#results-message");
const coverage = document.querySelector("#result-coverage");

let activeCall;
let activeSearch;
// Results currently on screen, numbered from 1, and the one the operator or agent is discussing.
let shownResults = [];
let selectedRef = 0;

const DESCRIPTION_LIMIT = 1500;

const voiceLabels = {
  idle: "Ready",
  connecting: "Connecting",
  listening: "Listening",
  speaking: "Speaking",
  muted: "Mic muted",
  ending: "Ending",
  error: "Unavailable",
};

const defaultHint = voiceHint.textContent;

function displayVoice(state, error = "") {
  const connected = ["listening", "speaking", "muted", "ending"].includes(state);
  voicePanel.dataset.state = state;
  voiceState.textContent = voiceLabels[state];
  voiceError.textContent = error;
  voiceError.hidden = !error;
  voiceHint.textContent = connected ? "You can interrupt or refine your request at any time." : defaultHint;
  startCallButton.hidden = connected;
  startCallButton.disabled = state === "connecting";
  startCallButton.textContent = state === "connecting" ? "Connecting…" : "Start conversation";
  callActions.hidden = !connected;
  endCallButton.disabled = state === "ending";
  muteCallButton.disabled = state === "ending";
  muteCallButton.setAttribute("aria-pressed", String(Boolean(activeCall?.muted)));
}

function updateCallMode(call) {
  if (call !== activeCall || call.ending) return;
  displayVoice(call.muted ? "muted" : call.mode, call.error);
}

function appendTranscript({ role, message }) {
  if (typeof message !== "string" || !message.trim()) return;
  const line = document.createElement("li");
  line.dataset.speaker = role;
  const speaker = document.createElement("strong");
  speaker.textContent = role === "user" ? "You" : "Watchtower";
  line.append(speaker, message);
  transcript.append(line);
  transcriptEmpty.hidden = true;
  transcript.scrollTop = transcript.scrollHeight;
}

function correctTranscript({ original_agent_response, corrected_agent_response }) {
  const agentLines = Array.from(transcript.querySelectorAll('[data-speaker="agent"]'));
  const line = agentLines.findLast(item => item.lastChild.textContent === original_agent_response);
  if (line) line.lastChild.textContent = corrected_agent_response;
}

async function getConversationToken(signal) {
  const response = await fetch("/assistant/voice/session", { method: "POST", signal });
  const payload = await response.json().catch(() => null);
  if (!response.ok || typeof payload?.conversationToken !== "string") {
    throw new Error(payload?.detail || "Watchtower could not start a voice session.");
  }
  return payload.conversationToken;
}

function clearCall(call, error = "") {
  if (call !== activeCall) return;
  call.controller.abort();
  activeCall = undefined;
  displayVoice(error ? "error" : "idle", error);
}

function callError(error) {
  if (error.name === "NotAllowedError" || error.name === "PermissionDeniedError") {
    return "Microphone access was denied. Allow it in your browser and try again.";
  }
  if (error.name === "NotFoundError") return "Connect a microphone and try again.";
  return error.message || "The voice connection could not be started.";
}

async function startCall() {
  if (activeCall) return;
  if (!navigator.mediaDevices?.getUserMedia) {
    displayVoice("error", "Voice needs microphone access from localhost or HTTPS.");
    return;
  }

  const call = { controller: new AbortController(), mode: "listening", muted: false, error: "" };
  activeCall = call;
  displayVoice("connecting");

  try {
    const conversationToken = await getConversationToken(call.controller.signal);
    const sdk = await import("./elevenlabs-sdk.js").catch(() => {
      throw new Error("The ElevenLabs SDK is unavailable. Build the dashboard assets and reload.");
    });
    if (call !== activeCall) return;

    transcript.replaceChildren();
    transcriptEmpty.hidden = false;
    const conversation = await sdk.Conversation.startSession({
      conversationToken,
      connectionType: "webrtc",
      stripAudioTags: true,
      onMessage: message => {
        if (call === activeCall) appendTranscript(message);
      },
      onAgentResponseCorrection: event => {
        if (call === activeCall) correctTranscript(event);
      },
      onModeChange: ({ mode }) => {
        call.mode = mode === "speaking" ? "speaking" : "listening";
        if (call.conversation) updateCallMode(call);
      },
      onError: () => {
        call.error = "The voice session encountered an error. End the call and try again.";
        if (call.conversation) updateCallMode(call);
      },
      onDisconnect: ({ reason } = {}) => {
        const error = reason === "error" && !call.ending ? "The voice connection ended unexpectedly." : "";
        clearCall(call, error);
      },
      clientTools: {
        search_tenders: async input => {
          if (call !== activeCall || call.ending) return JSON.stringify({ error: "The voice session has ended." });
          const result = await runSearch(input ?? {}, call.controller.signal);
          return JSON.stringify(result);
        },
        get_tender: async input => {
          if (call !== activeCall || call.ending) return JSON.stringify({ error: "The voice session has ended." });
          const result = await lookupTender(input ?? {}, call.controller.signal);
          return JSON.stringify(result);
        },
        explain_portal: async input => {
          if (call !== activeCall || call.ending) return JSON.stringify({ error: "The voice session has ended." });
          const result = await explainPortal(input ?? {}, call.controller.signal);
          return JSON.stringify(result);
        },
      },
    });

    if (call !== activeCall) {
      await conversation.endSession();
      return;
    }
    call.conversation = conversation;
    updateCallMode(call);
  } catch (error) {
    clearCall(call, callError(error));
  }
}

async function endCall() {
  const call = activeCall;
  if (!call?.conversation || call.ending) return;
  call.ending = true;
  displayVoice("ending");
  try {
    await call.conversation.endSession();
    clearCall(call);
  } catch {
    call.ending = false;
    call.error = "Could not end the call. Try ending it again.";
    updateCallMode(call);
  }
}

function safeUrl(href, isFile = false) {
  if (!href) return null;
  try {
    const url = new URL(href, location.origin);
    if (isFile) {
      return url.origin === location.origin && url.pathname.startsWith("/files/") ? url.href : null;
    }
    return /^https?:\/\//i.test(href) ? url.href : null;
  } catch {
    return null;
  }
}

function appendLink(container, href, label) {
  if (!href) return;
  const link = document.createElement("a");
  link.href = href;
  link.target = "_blank";
  link.rel = "noopener noreferrer";
  link.textContent = label;
  container.append(link);
}

function tenderCard(tender, index) {
  const ref = index + 1;
  const card = document.createElement("li");
  card.dataset.ref = String(ref);
  const heading = document.createElement("h4");
  const number = document.createElement("span");
  number.className = "tender-ref";
  number.textContent = ref + ".";
  heading.append(number, " ", tender.title || "Untitled tender");
  const facts = document.createElement("p");
  const deadline = tender.submissionDeadline?.replace("T", " ");
  facts.textContent = [
    tender.buyerName,
    tender.placeOfPerformance?.countryCode,
    deadline ? "Closes " + deadline : "Deadline unavailable",
    tender.portal.state === "healing" ? "Last verified data" : "",
  ].filter(Boolean).join(" · ");

  const links = document.createElement("p");
  links.className = "tender-links";
  appendLink(links, safeUrl(tender.url), "Source notice");
  for (const file of tender.documents ?? []) {
    appendLink(links, safeUrl(file.path, true), file.fileName || "Document");
  }
  const select = document.createElement("button");
  select.type = "button";
  select.className = "tender-select";
  select.textContent = "Ask about this";
  select.setAttribute("aria-pressed", "false");
  select.addEventListener("click", () => selectTender(selectedRef === ref ? 0 : ref, { announce: true }));
  links.append(select);
  card.append(heading, facts, links);
  return card;
}

function selectTender(ref, { announce = false } = {}) {
  selectedRef = shownResults[ref - 1] ? ref : 0;
  for (const card of tenderResults.children) {
    const selected = card.dataset.ref === String(selectedRef);
    card.toggleAttribute("data-selected", selected);
    card.querySelector(".tender-select")?.setAttribute("aria-pressed", String(selected));
  }

  const call = activeCall;
  if (!announce || !call?.conversation || call.ending) return;
  const entry = shownResults[selectedRef - 1];
  call.conversation.sendContextualUpdate(entry
    ? `The operator selected result ${selectedRef}: "${entry.title || "Untitled tender"}". Questions such as "who is the buyer" now refer to this tender; use get_tender to answer them.`
    : "The operator cleared the selected tender.");
}

/** Picks the tender the agent asked about: a result number, a tender ID, the operator's selection, or the only result. */
function resolveTender({ ref, tender_id: tenderId, portal_id: portalId }) {
  const number = Number.parseInt(String(ref ?? "").replace(/\D/g, ""), 10);
  if (number) return shownResults[number - 1] ?? { error: `There is no result ${number} on screen. Results 1 to ${shownResults.length} are shown.` };

  const id = String(tenderId ?? "").trim();
  if (id) {
    const shown = shownResults.find(item => item.id === id && (!portalId || item.portal.id === portalId));
    if (shown) return shown;
    if (portalId) return { id, portal: { id: String(portalId) }, key: "id:" + id };
    return { error: "That tender is not among the results on screen. Search for it first." };
  }

  if (selectedRef) return shownResults[selectedRef - 1];
  if (shownResults.length === 1) return shownResults[0];
  return { error: shownResults.length ? "Ask the operator which result number they mean." : "Search for tenders first." };
}

function tenderDetail(tender, portal, lastSuccessAt, ref) {
  const description = tender.description?.trim();
  const place = tender.placeOfPerformance ?? {};
  const detail = {
    ref: ref ? String(ref) : undefined,
    id: tender.id,
    title: tender.title,
    buyer: tender.buyerName,
    status: tender.status,
    reference: tender.referenceNumber,
    published: tender.publicationDate,
    closes: tender.submissionDeadline,
    procedure: tender.procedureType,
    nature: tender.contractNature,
    cpvCodes: tender.cpvCodes,
    city: place.city,
    postalCode: place.postalCode,
    country: place.countryCode,
    description: description && description.length > DESCRIPTION_LIMIT ? description.slice(0, DESCRIPTION_LIMIT) + "…" : description,
    descriptionTruncated: description?.length > DESCRIPTION_LIMIT || undefined,
    documents: (tender.documents ?? []).map(file => ({ name: file.fileName, sizeBytes: file.sizeBytes })),
    source: safeUrl(tender.url),
    lastChangedAt: tender.lastChangedAt,
    portal: { ...portal, lastVerifiedAt: lastSuccessAt },
  };
  const expected = { buyer: "buyer", closes: "closing time", description: "description", cpvCodes: "CPV codes", country: "country of performance", reference: "reference number" };
  detail.notPublished = Object.entries(expected).filter(([field]) => detail[field] == null || detail[field] === "").map(([, label]) => label);
  return detail;
}

async function lookupTender(input, callSignal) {
  const target = resolveTender(input);
  if (target.error) return { error: target.error };
  const key = target.key ?? tenderKey(target);
  if (!key) return { error: "That result has neither an ID nor a source URL to look it up by." };

  try {
    const { tender, portal, lastSuccessAt } = await findTender(target.portal.id, key, callSignal);
    const ref = shownResults.indexOf(target) + 1;
    if (ref) selectTender(ref);
    return tenderDetail(tender, portal, lastSuccessAt, ref);
  } catch (error) {
    return { error: callSignal.aborted ? "The voice session has ended." : error.message };
  }
}

const NAME_FILLER = new Set(["a", "an", "the", "of", "portal", "site", "website"]);

async function getJson(path, signal) {
  const response = await fetch(path, { cache: "no-store", signal });
  if (!response.ok) throw new Error(`Watchtower answered HTTP ${response.status} for ${path}.`);
  return response.json();
}

/** Finds portals by exact ID, or by name words that all appear in the portal URL, such as "Find a Tender". */
function matchPortals(portals, query) {
  const exact = portals.filter(portal => portal.id === query);
  if (exact.length) return exact;
  const words = query.toLowerCase().split(/[^a-z0-9]+/).filter(word => word && !NAME_FILLER.has(word));
  if (!words.length) return [];
  return portals.filter(portal => {
    const url = portal.url.toLowerCase().replace(/^https?:\/\//, "");
    const compact = url.replace(/[^a-z0-9]/g, "");
    return words.every(word => url.includes(word)) || compact.includes(words.join(""));
  });
}

function candidateFacts(candidate) {
  return {
    method: candidate.method,
    entryUrl: candidate.entryUrl,
    foundVia: candidate.foundVia,
    httpStatus: candidate.httpStatus,
    sampleItems: candidate.sampleItems,
    fieldsSeen: candidate.fieldsSeen,
    hasPagination: candidate.hasPagination,
    score: candidate.score,
  };
}

/** Read-only: a portal's delivery state and how Watchtower connects to it. */
async function explainPortal({ portal: query }, signal) {
  const wanted = String(query ?? "").trim();
  if (!wanted) return { error: "Say which portal to explain, by name or ID." };
  try {
    const portals = await getJson("/portals", signal);
    const known = portals.map(portal => ({ id: portal.id, url: portal.url, state: portal.state }));
    const matches = matchPortals(portals, wanted);
    if (!matches.length) return { error: `No registered portal matches "${wanted}".`, registeredPortals: known };
    if (matches.length > 1) {
      return { error: `Several portals match "${wanted}". Ask the operator which one they mean.`, matches: matches.map(portal => ({ id: portal.id, url: portal.url })) };
    }
    const base = "/portals/" + encodeURIComponent(matches[0].id);
    const [portal, decision] = await Promise.all([getJson(base, signal), getJson(base + "/connection", signal)]);
    return {
      id: portal.id,
      url: portal.url,
      state: portal.state,
      reason: portal.reason,
      lastSuccessAt: portal.lastSuccessAt,
      tenderCount: portal.tenderCount,
      connection: decision
        ? {
            chosen: candidateFacts(decision.chosen),
            rationale: decision.rationale,
            model: decision.model,
            decidedAt: decision.decidedAt,
            attempt: decision.attempt,
            alternatives: decision.alternatives.map(candidateFacts),
          }
        : null,
      connectionNote: decision ? undefined : "No connection decision is recorded for this portal yet.",
    };
  } catch (error) {
    return { error: signal.aborted ? "The voice session has ended." : error.message };
  }
}

function showSearch(payload) {
  const { results, total, searchedPortals, unavailablePortals, portalsWithoutData, countries, filters } = payload;
  searchForm.elements.query.value = filters.query;
  searchForm.elements.country.value = filters.country;
  shownResults = results;
  selectedRef = 0;
  tenderResults.replaceChildren(...results.map(tenderCard));
  coverage.textContent = [
    searchedPortals === 1 ? "1 portal searched" : searchedPortals + " portals searched",
    portalsWithoutData ? portalsWithoutData + " without published data" : "",
  ].filter(Boolean).join(" · ");
  resultsEmpty.hidden = results.length > 0;
  if (!searchedPortals && portalsWithoutData) {
    resultsMessage.textContent = "No connected portal has published tenders yet. Check portal status above.";
  } else if (!searchedPortals) {
    resultsMessage.textContent = "Connect a portal to search its published tenders.";
  } else if (filters.country && !countries.includes(filters.country)) {
    resultsMessage.textContent = "The searched portals have no published tenders marked " + filters.country + ".";
  } else {
    resultsMessage.textContent = "No open tenders matched these filters.";
  }

  const notes = [];
  if (total) notes.push(total > results.length ? "Showing " + results.length + " of " + total + " matches." : total + (total === 1 ? " match." : " matches."));
  if (payload.tendersWithoutCpv) {
    const skipped = payload.tendersWithoutCpv;
    notes.push(skipped + (skipped === 1 ? " open tender has" : " open tenders have") + " no CPV codes, so the sector filter skipped "
      + (skipped === 1 ? "it" : "them") + " (" + payload.portalsWithoutCpv.join(", ") + ").");
  }
  if (unavailablePortals) notes.push("Tender data unavailable from " + unavailablePortals + (unavailablePortals === 1 ? " portal." : " portals."));
  const sector = filters.cpv.length
    && (filters.sector ? "sector " + filters.sector + " (CPV " + filters.cpv.join(", ") + ")" : "CPV " + filters.cpv.join(", "));
  const voiceFilters = [sector, filters.deadlineFrom && "closing from " + filters.deadlineFrom, filters.deadlineTo && "closing by " + filters.deadlineTo].filter(Boolean);
  if (voiceFilters.length) notes.push("Filters: " + voiceFilters.join(", ") + ".");
  searchStatus.textContent = notes.join(" ");
}

async function runSearch(input, callSignal) {
  activeSearch?.abort();
  const search = new AbortController();
  activeSearch = search;
  const signal = callSignal ? AbortSignal.any([callSignal, search.signal]) : search.signal;

  tenderResults.replaceChildren();
  shownResults = [];
  selectedRef = 0;
  resultsEmpty.hidden = true;
  coverage.textContent = "";
  searchStatus.textContent = "Searching published tenders…";

  try {
    const payload = await searchOpenTenders(input, signal);
    signal.throwIfAborted();
    showSearch(payload);
    const { total, searchedPortals, unavailablePortals, portalsWithoutData, portals, countries, results, filters, tendersWithoutCpv, portalsWithoutCpv } = payload;
    return {
      total,
      sector: filters.sector || undefined,
      cpvPrefixes: filters.cpv.length ? filters.cpv : undefined,
      tendersWithoutCpv: filters.cpv.length ? tendersWithoutCpv : undefined,
      portalsWithoutCpv: tendersWithoutCpv ? portalsWithoutCpv : undefined,
      searchedPortals,
      unavailablePortals,
      portalsWithoutData,
      portals,
      countries,
      tenders: results.map((tender, index) => ({
        ref: String(index + 1),
        id: tender.id,
        title: tender.title,
        buyer: tender.buyerName,
        country: tender.placeOfPerformance?.countryCode,
        closes: tender.submissionDeadline,
        cpvCodes: tender.cpvCodes,
        source: safeUrl(tender.url),
      })),
    };
  } catch (error) {
    const message = signal.aborted ? "Search ended." : error.message;
    if (search === activeSearch) {
      resultsEmpty.hidden = false;
      resultsMessage.textContent = message;
      searchStatus.textContent = "";
    }
    return { error: message };
  } finally {
    if (search === activeSearch) activeSearch = undefined;
  }
}

async function closeAssistant() {
  const call = activeCall;
  if (call?.conversation) {
    await endCall();
    if (activeCall) return;
  } else if (call) {
    clearCall(call);
  }
  assistantDialog.close();
}

document.querySelector("#open-assistant").addEventListener("click", () => assistantDialog.showModal());
document.querySelector("#close-assistant").addEventListener("click", closeAssistant);
assistantDialog.addEventListener("cancel", event => {
  event.preventDefault();
  void closeAssistant();
});
startCallButton.addEventListener("click", startCall);
endCallButton.addEventListener("click", endCall);
muteCallButton.addEventListener("click", () => {
  const call = activeCall;
  if (!call?.conversation || call.ending) return;
  call.muted = !call.muted;
  call.conversation.setMicMuted(call.muted);
  updateCallMode(call);
});
searchForm.addEventListener("submit", event => {
  event.preventDefault();
  const filters = new FormData(searchForm);
  void runSearch({ query: filters.get("query"), country: filters.get("country") });
});
window.addEventListener("pagehide", () => {
  const call = activeCall;
  if (!call) return;
  clearCall(call);
  void call.conversation?.endSession().catch(() => {});
});
