-- A rewound conversation (ADR 0009): the transcript entry its next session
-- resumes at. Cleared once the agent answers on the new branch.
ALTER TABLE workflows ADD COLUMN resume_at TEXT;
