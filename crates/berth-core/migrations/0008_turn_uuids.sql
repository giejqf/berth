-- Each round's opening turn as Claude Code's transcript knows it (the uuid
-- Berth gives the user message), and the turn a pending rewind drops: a
-- resume then declares it, and Claude Code refuses to drop anything else
-- (ADR 0016).
ALTER TABLE rounds ADD COLUMN turn_uuid TEXT;
ALTER TABLE workflows ADD COLUMN resume_drops TEXT;
