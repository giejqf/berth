-- Files the user marked as viewed in the review pane, by the change they
-- read: `change` is the blob ids before and after ("<old>..<new>"), so a
-- file the agent changes again counts as not viewed.
CREATE TABLE viewed_files (
    workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    change      TEXT NOT NULL,
    viewed_at   INTEGER NOT NULL,
    PRIMARY KEY (workflow_id, path, change)
);
