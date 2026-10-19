-- The slot's agent settings; NULL is the default (the app's model setting,
-- the model's own effort).
ALTER TABLE slots ADD COLUMN model TEXT;
ALTER TABLE slots ADD COLUMN effort TEXT;
