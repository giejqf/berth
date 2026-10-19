-- Which window shows the slot (ADR 0011): a repo's slots are grouped into
-- windows, each window one group. Assigned when a slot is created or moved.
ALTER TABLE slots ADD COLUMN window_id TEXT;
