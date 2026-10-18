-- Berth schema v1 (AGENTS.md §9). Timestamps are unix epoch milliseconds.

CREATE TABLE repos (
    id                TEXT PRIMARY KEY,
    path              TEXT NOT NULL UNIQUE,
    default_slot_type TEXT,
    created_at        INTEGER NOT NULL
);

CREATE TABLE slots (
    id            TEXT PRIMARY KEY,
    repo_id       TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    type          TEXT NOT NULL,
    name          TEXT NOT NULL,
    state         TEXT NOT NULL,          -- creating | ready | running | destroying | destroyed | failed
    state_detail  TEXT,
    checkout_path TEXT,
    placement     TEXT NOT NULL,          -- host | inside
    branch        TEXT NOT NULL,
    base_ref      TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);
CREATE INDEX slots_repo ON slots(repo_id);

CREATE TABLE workflows (
    id                TEXT PRIMARY KEY,
    slot_id           TEXT NOT NULL REFERENCES slots(id) ON DELETE CASCADE,
    origin_json       TEXT NOT NULL,
    state             TEXT NOT NULL,      -- starting | running | awaiting_answer | idle | closed
    claude_session_id TEXT,
    section_no        INTEGER NOT NULL DEFAULT 1,
    total_cost_usd    REAL NOT NULL DEFAULT 0,
    last_error        TEXT,
    created_at        INTEGER NOT NULL,
    closed_at         INTEGER
);
CREATE INDEX workflows_slot ON workflows(slot_id);

CREATE TABLE rounds (
    id            TEXT PRIMARY KEY,
    workflow_id   TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    section_no    INTEGER NOT NULL,
    started_at    INTEGER NOT NULL,
    ended_at      INTEGER,
    terminal_kind TEXT,                   -- write_summary | record_outcome | unterminated | interrupted | error
    nudges        INTEGER NOT NULL DEFAULT 0,
    cost_usd      REAL NOT NULL DEFAULT 0,
    duration_ms   INTEGER
);
CREATE INDEX rounds_workflow ON rounds(workflow_id, started_at);

CREATE TABLE branches (
    id                 TEXT PRIMARY KEY,
    workflow_id        TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    spawn_round_id     TEXT REFERENCES rounds(id),
    collect_round_id   TEXT REFERENCES rounds(id),
    parent_branch_id   TEXT REFERENCES branches(id),
    parent_tool_use_id TEXT NOT NULL,
    agent_name         TEXT NOT NULL,
    description        TEXT NOT NULL,
    parallel_group     TEXT,
    status             TEXT NOT NULL,     -- running | ok | error | cancelled
    spawned_at         INTEGER NOT NULL,
    collected_at       INTEGER,
    cost_usd           REAL,
    spawn_tree         TEXT,
    collect_tree       TEXT,
    result_json        TEXT
);
CREATE INDEX branches_workflow ON branches(workflow_id);

CREATE TABLE nodes (
    id           TEXT PRIMARY KEY,
    workflow_id  TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    round_id     TEXT REFERENCES rounds(id),
    branch_id    TEXT REFERENCES branches(id),
    seq          INTEGER NOT NULL,        -- order within the workflow
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    raw_first    INTEGER,
    raw_last     INTEGER,
    created_at   INTEGER NOT NULL
);
CREATE UNIQUE INDEX nodes_order ON nodes(workflow_id, seq);

CREATE TABLE raw_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    round_id    TEXT REFERENCES rounds(id),
    branch_id   TEXT,
    seq         INTEGER NOT NULL,         -- sidecar seq (resets per sidecar process)
    kind        TEXT NOT NULL,
    json        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX raw_events_workflow ON raw_events(workflow_id, id);

CREATE TABLE snapshots (
    id          TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    section_no  INTEGER NOT NULL,
    kind        TEXT NOT NULL,            -- start | summary
    tree_sha    TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX snapshots_workflow ON snapshots(workflow_id, section_no);

CREATE TABLE review_comments (
    id            TEXT PRIMARY KEY,
    workflow_id   TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    from_snapshot TEXT NOT NULL,          -- tree sha or ref
    to_snapshot   TEXT NOT NULL,
    preset        TEXT NOT NULL,
    path          TEXT NOT NULL,
    side          TEXT NOT NULL,          -- old | new
    line          INTEGER NOT NULL,
    body          TEXT NOT NULL,
    sent_in_node  TEXT,                   -- Prompt node that delivered it to the agent
    posted_to_pr  INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);
CREATE INDEX review_comments_workflow ON review_comments(workflow_id);

CREATE TABLE gh_entities (
    id            TEXT PRIMARY KEY,
    repo_id       TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,          -- pr | issue | branch | commit | comment
    number_or_sha TEXT NOT NULL,
    json          TEXT NOT NULL,
    synced_at     INTEGER NOT NULL,
    UNIQUE (repo_id, kind, number_or_sha)
);

CREATE TABLE workflow_links (
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    entity_id   TEXT NOT NULL REFERENCES gh_entities(id) ON DELETE CASCADE,
    role        TEXT NOT NULL,            -- origin | outcome
    PRIMARY KEY (workflow_id, entity_id, role)
);

CREATE TABLE connectors (
    name    TEXT PRIMARY KEY,
    path    TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
