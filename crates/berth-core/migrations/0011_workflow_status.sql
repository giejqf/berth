-- A status the user gave the conversation by hand (todo, needs_you, review,
-- done), and when. It holds until anything newer happens in the workflow: a
-- node, a round starting or ending.
ALTER TABLE workflows ADD COLUMN status TEXT;
ALTER TABLE workflows ADD COLUMN status_at INTEGER;
