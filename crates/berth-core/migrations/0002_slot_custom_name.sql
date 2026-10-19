-- A name the user gave the slot; the rail shows it instead of the workflow's title.
ALTER TABLE slots ADD COLUMN custom_name TEXT;
