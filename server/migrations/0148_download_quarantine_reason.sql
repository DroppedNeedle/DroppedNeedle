-- Which check blocklisted a source and what it found: the acquire
-- reason's stored spelling and the landing's detail text. NULL on every
-- carried row, which keeps the general sentence of its blocklist reason.

ALTER TABLE download_quarantine ADD COLUMN reason_code TEXT;
ALTER TABLE download_quarantine ADD COLUMN detail TEXT;

PRAGMA user_version = 148;
