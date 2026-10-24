-- Each slot's number in its repo, as the rail shows it ("2"): the lowest one
-- no other live slot of the repo holds, kept for the slot's life. Slots that
-- exist now are numbered in the order they were made; destroyed ones keep 0.
ALTER TABLE slots ADD COLUMN number INTEGER NOT NULL DEFAULT 0;
UPDATE slots SET number = (
    SELECT COUNT(*) FROM slots s
    WHERE s.repo_id = slots.repo_id AND s.state != 'destroyed'
      AND (s.created_at < slots.created_at OR (s.created_at = slots.created_at AND s.id <= slots.id))
) WHERE state != 'destroyed';
