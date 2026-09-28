# Capture 02 — Identity decisions

## What it pins

How v2 decides *what an album is* (and who owns the decision):

- **Fold/distance/classifier samples:** live-run `_fold`, `_distance`,
  `_album_title_class`, `_album_artist_class` (both with no proof and with full
  MBID proof, pinning the proof-gated subset escape), and `_artist_subset_match`
  over 10 adversarial string pairs (diacritics, sort-order flips, edition
  suffixes, subset credits).
- **Edition-uncertainty samples:** live-run `is_edition_uncertain()` over three
  `IdentificationDecision` shapes (accepted/decisive, needs-review/thin-margin,
  rejected) with full serialized decisions.
- **MBID-proof samples:** `_has_local_track_mbid()` over full-proof vs no-proof
  track sets.
- **Identity table DDL:** verbatim `CREATE TABLE` text for
  `local_{artist,album,track}_external_identities`,
  `library_identification_attempts`, and `library_identification_evidence`,
  pinning the `decision_source IN ('embedded','automatic','manual',
  'legacy_import')` domains and protection semantics.
- **Scenario catalog:** every `test_*` name + docstring first line from the
  identification-pipeline, evidence-engine (incl. fold), and
  artist-reconciliation suites — the behavioral checklist v3 must re-satisfy.

Sampled automatic/manual/legacy outcomes are represented by the decision shapes
plus the cataloged pipeline scenarios (the pipeline itself needs MusicBrainz +
  a live DB, so full end-to-end runs are cataloged, not executed — see
  top-level README).

## Method

Live-run pure functions + static pins: `/tmp/vcap/cap02_identity.py`
(`cd backend && .venv/bin/python /tmp/vcap/cap02_identity.py`). Read-only.

## Consumed by (v3)

- **Stage 2 (identity engine):** classifier/uncertainty parity — same inputs
  must yield same classes and same `edition_uncertain` flags.
- **Stage 3 (reconciliation):** DDL + `decision_source` protection rules seed
  the v3 catalog schema; scenario catalog becomes the v3 identity test plan.
