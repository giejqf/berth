-- When a workflow was opened from the repo's history into its slot (ADR 0010):
-- a slot shows the workflow it hosted last, opened or created.
ALTER TABLE workflows ADD COLUMN opened_at INTEGER;
