import dashboard from '../src/dashboard.html?raw';
import stylesheet from '../src/dashboard.css?raw';
import assistantSource from '../src/dashboard/assistant.js?raw';
import searchSource from '../src/dashboard/tender-search.js?raw';

// The Rust server serves the assistant at /dashboard/assistant.js. Here the same modules load
// from blob URLs, so the dialog opens and searches the fixtures below. Starting a voice call
// still fails, because Storybook has no voice SDK or session endpoint.
const moduleUrl = source => URL.createObjectURL(new Blob([source], { type: 'text/javascript' }));
const assistantUrl = moduleUrl(assistantSource.replace('"./tender-search.js"', JSON.stringify(moduleUrl(searchSource))));

const fileText = 'Park lighting renewal\nReplace the lighting on the riverside footpath.\n';
const digest = 'eec2aefd7c37284c92494b52baff6e0d6ae4ea884d0791a7f4f9ad37b187475c';
const specification = {
  fileName: 'Lighting specification.txt',
  sizeBytes: new TextEncoder().encode(fileText).length,
  sha256: digest,
  path: '/files/' + digest,
};

const fleet = [
  {
    id: 'westmere',
    url: 'https://procurement.westmere.example/tenders',
    state: 'serving',
    reason: 'The latest read verified all listed tenders and their documents.',
    tenderCount: 2,
    lastSuccessAt: '2026-11-20T10:00:00Z',
  },
  {
    id: 'northport',
    url: 'https://contracts.northport.example/opportunities',
    state: 'healing',
    reason: 'The attachment listing changed. The previous verified documents remain available during repair.',
    tenderCount: 1,
    lastSuccessAt: '2026-11-20T09:30:00Z',
  },
  {
    id: 'alderwick',
    url: 'https://tenders.alderwick.example/notices',
    state: 'integrating',
    reason: 'Reading the public listing and checking its first set of tenders.',
    tenderCount: 0,
    lastSuccessAt: null,
  },
  {
    id: 'eastborough',
    url: 'https://bids.eastborough.example/current',
    state: 'failed',
    reason: 'The portal requires a login to download tender documents.',
    tenderCount: 0,
    lastSuccessAt: null,
  },
];

const tender = (portal, id, title, buyerName, deadline, documents = []) => ({
  id, title, buyerName, documents,
  url: portal.url + '/' + id,
  publicationDate: '2026-11-09',
  submissionDeadline: deadline,
  status: deadline < '2026-11-20' ? 'closed' : 'open',
  lastChangedAt: portal.lastSuccessAt,
});

const tenders = {
  westmere: [
    tender(fleet[0], 'WM-26-041', 'Park lighting', 'Westmere Borough Council', '2026-12-14T12:00:00Z', [specification]),
    tender(fleet[0], 'WM-26-033', 'Bridge repairs', 'Westmere Borough Council', '2026-11-17T12:00:00Z'),
  ],
  northport: [
    tender(fleet[1], 'NP-26-118', 'Harbour wall inspection', 'Northport Harbour Authority', '2026-12-08T12:00:00Z'),
  ],
};

function run(id, kind, started, finished, result) {
  return {
    id, kind, result,
    startedAt: '2026-11-20T' + started + ':00Z',
    finishedAt: finished ? '2026-11-20T' + finished + ':00Z' : null,
  };
}

const runs = {
  westmere: [
    run('w1', 'read', '09:30', '09:31', 'ok'),
    run('w2', 'read', '09:59', '10:00', 'ok'),
  ],
  northport: [
    run('n1', 'read', '09:29', '09:30', 'ok'),
    run('n2', 'read', '10:01', '10:02', 'failed'),
    run('n3', 'repair', '10:02', null, null),
  ],
  alderwick: [run('a1', 'read', '10:03', null, null)],
  eastborough: [run('e1', 'read', '09:45', '09:46', 'failed')],
};

// `GET /portals/{id}/connection`: the probed methods and the one Watchtower chose.
function candidate(method, entryUrl, score, httpStatus, sampleItems, fieldsSeen, foundVia = 'start page link') {
  return {
    method, entryUrl, foundVia, httpStatus, contentType: null, sampleItems, fieldsSeen,
    hasPagination: sampleItems > 10, score,
  };
}
const decision = (portal, chosen, alternatives, rationale, attempt = 1) => ({
  chosen, alternatives, rationale, attempt,
  model: 'gpt-5-mini',
  decidedAt: portal.lastSuccessAt ?? '2026-11-20T09:40:00Z',
});
const connections = {
  westmere: decision(fleet[0],
    candidate('ocds', 'https://procurement.westmere.example/api/ocds/releases', 0.91, 200, 25, ['title', 'deadline', 'buyer', 'documents'], 'link rel=alternate'),
    [candidate('html', 'https://procurement.westmere.example/tenders', 0.62, 200, 20, ['title', 'deadline'], 'start page')],
    'The OCDS endpoint lists every open tender with deadlines, buyers and document links, and pages with a cursor. The HTML listing shows the same tenders with fewer fields.'),
  northport: decision(fleet[1],
    candidate('html', 'https://contracts.northport.example/opportunities', 0.74, 200, 42, ['title', 'deadline', 'buyer'], 'start page'),
    [candidate('feed', 'https://contracts.northport.example/opportunities.rss', 0.41, 200, 10, ['title'], 'link rel=alternate')],
    'Only the HTML listing carries deadlines and buyers; the RSS feed has titles and links only.', 2),
  alderwick: null,
  eastborough: decision(fleet[3],
    candidate('html', 'https://bids.eastborough.example/current', 0.55, 200, 8, ['title'], 'start page'),
    [candidate('json', 'https://bids.eastborough.example/api/notices', 0.2, 401, 0, [], 'script source')],
    'The listing page is public, but its document links lead to a sign-in page and the JSON API answers 401.'),
};

// The markup `/dashboard/portals/{id}/history` renders (src/dashboard.rs), newest first.
const VERBS = { ok: 'succeeded', failed: 'failed', running: 'running' };
function step(kind, result, started, role, reason) {
  const name = (kind === 'repair' ? 'Repair ' : 'Read ') + VERBS[result];
  return '<div class="run-step" data-kind="' + kind + '" data-result="' + result + '">'
    + '<span class="record-name">' + name + (role ? ' · ' + role : '') + '</span>'
    + '<time class="record-time" datetime="2026-11-20T' + started + ':00Z"></time>'
    + (reason ? '<p class="run-reason">' + reason + '</p>' : '') + '</div>';
}
const list = items => '<ul class="record-list run-history">' + items.join('') + '</ul>';
const history = {
  westmere: list([
    '<li>' + step('read', 'ok', '09:59', '', 'Read verified. 2 tenders held, no changes') + '</li>',
    '<li>' + step('read', 'ok', '09:30', '', 'Attached and verified. 2 tenders held, 2 changed') + '</li>',
  ]),
  northport: list([
    '<li class="run-pair">'
      + step('read', 'failed', '10:01', 'what broke', 'Listing returned 0 rows for items ".row". The last verified read held 42 tenders, 40 open, and stays served')
      + step('repair', 'running', '10:02', 'repair in progress', '') + '</li>',
    '<li>' + step('read', 'ok', '09:29', '', 'Read verified. 42 tenders held, 1 changed') + '</li>',
  ]),
  alderwick: list(['<li>' + step('read', 'running', '10:03', '', '') + '</li>']),
  eastborough: list(['<li>' + step('read', 'failed', '09:45', '', 'Listing pages answer with a sign-in page; a login blocks delivery') + '</li>']),
};

function preview({ scenario }) {
  const portals = scenario === 'fleet' ? fleet : scenario === 'serving' ? [fleet[0]] : [];
  const openTenders = {};
  const responses = { '/portals': portals, '/dashboard/open-tenders': openTenders };
  for (const portal of portals) {
    const path = '/portals/' + portal.id;
    const open = (tenders[portal.id] ?? []).filter(item => item.status === 'open').length;
    if (open) openTenders[portal.id] = open;
    responses[path + '/connection'] = connections[portal.id] ?? null;
    responses[path + '/tenders'] = tenders[portal.id] ?? [];
    responses[path + '/runs'] = runs[portal.id] ?? [];
    responses['/dashboard' + path + '/history'] = history[portal.id] ?? '<p>Waiting for first read</p>';
  }
  const fixture = `<script>
    const fixtures = ${JSON.stringify(responses)};
    const file = ${JSON.stringify(specification)};
    const fileText = ${JSON.stringify(fileText)};
    function responseFor(path) {
      if (path === '/portals' && ${scenario === 'unavailable'}) {
        return new Response(null, { status: 503 });
      }
      if (path === file.path) return new Response(fileText, {headers: {'Content-Type': 'text/plain'}});
      if (Object.hasOwn(fixtures, path) && typeof fixtures[path] === 'string') {
        return new Response(fixtures[path], {headers: {'Content-Type': 'text/html'}});
      }
      if (Object.hasOwn(fixtures, path)) return Response.json(fixtures[path]);
      return new Response(null, {status: 404});
    }
    window.fetch = async path => responseFor(new URL(path, document.baseURI).pathname);

    // Real links use the Rust API. In Storybook, open the matching fixture locally.
    document.addEventListener('click', async event => {
      const link = event.target.closest('a[href]');
      const path = link?.getAttribute('href');
      // Fragment links inside srcdoc otherwise resolve against the Storybook host page.
      if (path?.startsWith('#')) {
        const section = document.getElementById(path.slice(1));
        if (section) {
          event.preventDefault();
          section.scrollIntoView();
        }
        return;
      }
      if (!path || (!Object.hasOwn(fixtures, path) && path !== file.path)) return;
      event.preventDefault();
      const destination = document.createElement('a');
      if (path === file.path) {
        destination.download = file.fileName;
      } else {
        destination.target = '_blank';
        destination.rel = 'noopener';
      }
      destination.href = URL.createObjectURL(await responseFor(path).blob());
      destination.click();
      setTimeout(() => URL.revokeObjectURL(destination.href), 60000);
    });
  </script>`;
  const frame = document.createElement('iframe');
  frame.title = 'Watchtower dashboard';
  frame.style.cssText = 'display:block;width:100%;height:100vh;border:0';
  frame.srcdoc = dashboard
    .replace('<script>', () => fixture + '<script>')
    .replace('<script type="module" src="/dashboard/assistant.js"></script>', () => `<script type="module" src="${assistantUrl}"></script>`)
    .replace(/<link rel="stylesheet" href="\/dashboard\.css(?:\?[^"]*)?">/, () => `<style>${stylesheet}</style>`);
  return frame;
}

export default {
  title: 'Watchtower/Dashboard',
  parameters: { layout: 'fullscreen' },
  render: preview,
};

export const Fleet = { args: { scenario: 'fleet' } };
export const Serving = { args: { scenario: 'serving' } };
export const Empty = { args: { scenario: 'empty' } };
export const Unavailable = { args: { scenario: 'unavailable' } };
