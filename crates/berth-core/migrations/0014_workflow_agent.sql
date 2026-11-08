-- Which agent a conversation runs on (ADR 0033): Claude Code, or Pi under an
-- OpenAI-compatible provider. Set when its first session starts; kept after.
ALTER TABLE workflows ADD COLUMN agent TEXT NOT NULL DEFAULT 'claude';
