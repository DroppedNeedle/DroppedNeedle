# fixtures/v2-capture — Stage-1 v2 behavior capture

Recorded from v2 **before deletion** so v3 stages can verify parity without the
v2 code. Branch: `v3`. Each capture JSON records its own `v2_sha` +
`captured_at`.

## Layout

| Dir | Pins | v3 consumer |
|---|---|---|
| `01-scan/` | fixture inventory (path/size/sha256), `AudioTagger` tag reads, filename parses, grouping/provenance helpers | Stage 2 (library engine), Stage 4 (corpus drift) |
| `02-identity/` | evidence classifiers, edition-uncertainty, MBID-proof, identity DDL, scenario catalog (253 cases) | Stage 2 (identity engine), Stage 3 (reconciliation) |
| `03-compat/` | 48 Subsonic+Jellyfin request/response goldens + streaming headers, self-golden MUST-MATCH manifest | Stage 3 (compat shims), Stage 4 (streaming) |
| `04-acquisition/` | slskd/SAB/Newznab mock transcripts as data (+ raw Newznab XML) | Stage 3 (acquisition clients) |
| `05-publisher/` | sealed-preview/undo tokens, stable JSON, gate vocabulary, scenario catalog (163 cases), exception inventory | Stage 3 (library management), Stage 4 (safety/recovery) |

Each directory has a README stating what it pins and which v3 stage consumes it.

## Rules honored

- **Read-only v2:** v2 code was *run*, never edited — no changes under
  `backend/`, no commits, no pushes, no `make rebuild`, no prod container/data
  contact. All runs used temp dirs (`mkdtemp`) plus the repo's committed
  fixtures.
- **New files only under `fixtures/v2-capture/`** (fresh dir, nothing else
  written in the repo; capture scripts lived in `/tmp/vcap/`, uncommitted).
- **Real-audio fixtures referenced by path** (`backend/tests/fixtures/library/…`
  + sha256); bytes never embedded; tag reads only, never tagger round-trips.

## Known capture limits (honest gaps, not failures)

- **02 end-to-end identification:** the full pipeline (MusicBrainz-backed album
  identification, artist merge execution, legacy migrator runs) needs network +
  a live catalog DB, so it is pinned as classifier/decision live-runs + DDL +
  the 253-case scenario catalog — not as executed pipeline transcripts.
- **05 end-to-end publish/undo:** same rationale — sealed-preview math is
  live-run; apply/undo/journal flows are pinned as the 163-case scenario
  catalog + failure alphabet.
- **03 stubs:** cover art (none → placeholder/404 paths), playlists (empty),
  now-playing (empty), random-songs (empty) — same minimalism as v2's own
  `streaming_env` fixture; every other response is real shim logic over a real
  FLAC on disk.
