-- The workflow's context window as its session last reported it (ADR 0014),
-- a `ContextUsage` as JSON, so the composer's ring shows it after a restart.
ALTER TABLE workflows ADD COLUMN context_json TEXT;
