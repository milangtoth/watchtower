import { searchOpenTenders } from "./tender-search.js";

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

function tenderCard(tender) {
  const card = document.createElement("li");
  const heading = document.createElement("h4");
  heading.textContent = tender.title || "Untitled tender";
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
  card.append(heading, facts, links);
  return card;
}

function showSearch(payload) {
  const { results, total, searchedPortals, unavailablePortals, portalsWithoutData, countries, filters } = payload;
  searchForm.elements.query.value = filters.query;
  searchForm.elements.country.value = filters.country;
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
  if (unavailablePortals) notes.push("Tender data unavailable from " + unavailablePortals + (unavailablePortals === 1 ? " portal." : " portals."));
  const voiceFilters = [filters.cpv && "CPV " + filters.cpv, filters.deadlineFrom && "closing from " + filters.deadlineFrom, filters.deadlineTo && "closing by " + filters.deadlineTo].filter(Boolean);
  if (voiceFilters.length) notes.push("Filters: " + voiceFilters.join(", ") + ".");
  searchStatus.textContent = notes.join(" ");
}

async function runSearch(input, callSignal) {
  activeSearch?.abort();
  const search = new AbortController();
  activeSearch = search;
  const signal = callSignal ? AbortSignal.any([callSignal, search.signal]) : search.signal;

  tenderResults.replaceChildren();
  resultsEmpty.hidden = true;
  coverage.textContent = "";
  searchStatus.textContent = "Searching published tenders…";

  try {
    const payload = await searchOpenTenders(input, signal);
    signal.throwIfAborted();
    showSearch(payload);
    const { total, searchedPortals, unavailablePortals, portalsWithoutData, portals, countries, results } = payload;
    return {
      total,
      searchedPortals,
      unavailablePortals,
      portalsWithoutData,
      portals,
      countries,
      tenders: results.map(tender => ({
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
