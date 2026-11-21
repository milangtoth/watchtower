CREATE TABLE IF NOT EXISTS portals (
    id TEXT PRIMARY KEY, url TEXT UNIQUE NOT NULL, generation INTEGER NOT NULL DEFAULT 1,
    deleted INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL DEFAULT 'integrating',
    reason TEXT NOT NULL DEFAULT 'Waiting for the first read', tender_count INTEGER NOT NULL DEFAULT 0,
    last_success_at TEXT, recipe TEXT, next_read_at INTEGER
);
CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY, portal_id TEXT NOT NULL, generation INTEGER NOT NULL, kind TEXT NOT NULL,
    started_at TEXT, finished_at TEXT, result TEXT, reason TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS one_pending_read ON runs(portal_id, generation)
    WHERE kind = 'read' AND finished_at IS NULL;
CREATE INDEX IF NOT EXISTS portal_runs ON runs(portal_id, generation);
CREATE TABLE IF NOT EXISTS tenders (
    portal_id TEXT NOT NULL, generation INTEGER NOT NULL, tender_key TEXT NOT NULL, payload TEXT NOT NULL,
    PRIMARY KEY (portal_id, generation, tender_key)
);
CREATE TABLE IF NOT EXISTS portal_files (
    portal_id TEXT NOT NULL, generation INTEGER NOT NULL, sha256 TEXT NOT NULL,
    PRIMARY KEY (portal_id, generation, sha256)
);
CREATE INDEX IF NOT EXISTS file_lookup ON portal_files(sha256);
CREATE TABLE IF NOT EXISTS model_usage (
    portal_id TEXT NOT NULL, generation INTEGER NOT NULL, operation TEXT NOT NULL, model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL, cached_input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS read_evidence (
    portal_id TEXT NOT NULL, generation INTEGER NOT NULL, payload TEXT NOT NULL,
    PRIMARY KEY (portal_id, generation)
);
CREATE TABLE IF NOT EXISTS portal_retry (
    portal_id TEXT NOT NULL, generation INTEGER NOT NULL,
    temporary_failures INTEGER NOT NULL DEFAULT 0, last_repair_at INTEGER, manual_run TEXT,
    PRIMARY KEY (portal_id, generation)
);

CREATE TABLE IF NOT EXISTS preferences (
    name TEXT PRIMARY KEY, enabled INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS connection_decisions (
    portal_id TEXT NOT NULL, generation INTEGER NOT NULL, attempt INTEGER NOT NULL, payload TEXT NOT NULL,
    PRIMARY KEY (portal_id, generation, attempt)
);
