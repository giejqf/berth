-- How many agent messages in a row, without the user, reached each round
-- (the highest that woke or joined it): a message its agent sends goes one
-- further, and one past the limit waits for the user (ADR 0021).
ALTER TABLE rounds ADD COLUMN hop INTEGER NOT NULL DEFAULT 0;
