# Capture 01 — Scan corpora + per-phase outcomes

## What it pins

How v2 turns library files into catalog rows, phase by phase:

- **Phase A (inventory):** the exact committed real-audio corpus —
  `repo_path`, byte size, and sha256 for every
  `backend/tests/fixtures/library/*.{flac,mp3,m4a,ogg,opus,wav,wma,aac}` file.
- **Phase B (tag reads):** `AudioTagger.read_tags()` outcomes (ok/error) plus the
  full serialized `AudioTag` + `AudioInfo` per fixture. This is the tag-reader
  contract v3 must reproduce (field names, types, lossless `bitrate=None`
  conventions, etc.).
- **Phase C (filename parses):** `parse_names_from_path()` outputs over a
  15-path corpus of representative library layouts (multi-disc, compilations,
  feat. credits, classical, deep nesting).
- **Phase D (pure helpers):** `_filename_track_number` / `_filename_title`,
  `grouping_directory`, and `parse_names_for_row` over the same corpus.

Real-audio fixtures are **referenced by repo path** (`repo_path`); bytes are
never embedded and no tagger round-trip was used (reads only).

## Method

Live-run, read-only: `/tmp/vcap/cap01_scan.py` (scratch, not committed) run as
`cd backend && .venv/bin/python /tmp/vcap/cap01_scan.py` against v2 at the SHA
recorded in `scan_corpora.json` (`v2_sha`). No backend file was modified; no
prod data was touched.

## Consumed by (v3)

- **Stage 2 (library engine):** tag-reader + filename-parser parity checks —
  v3 must produce identical Phase B/C/D outputs for the same corpus.
- **Stage 4 (compat/acceptance):** Phase A inventory hashes prove the corpus
  itself did not drift.
