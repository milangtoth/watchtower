import dashboard from '../src/dashboard.html?raw';
import stylesheet from '../src/dashboard.css?raw';

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

function preview({ scenario }) {
  const portals = scenario === 'fleet' ? fleet : scenario === 'serving' ? [fleet[0]] : [];
  const responses = { '/portals': portals };
  for (const portal of portals) {
    const path = '/portals/' + portal.id;
    responses[path + '/tenders'] = tenders[portal.id] ?? [];
    responses[path + '/runs'] = runs[portal.id] ?? [];
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
