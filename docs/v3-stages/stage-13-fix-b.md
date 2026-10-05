# Stage 13 fix step B — shutdown check in the identify drain

Status: ACCEPTED 2026-10-04. Branch `v3`. Gate source:
`docs/v3-stages/stage-13-budgets.md` row 3 (FAIL) + F2.

## Manifest

Scope: `identify_tick` drains every due job with no shutdown
check, so a SIGTERM landing mid-drain waits out thousands of
1/s MusicBrainz-gated attempts (measured: past 120 s to
SIGKILL). Add a shutdown check to the drain loop so quiet and
mid-drain shutdowns both exit cleanly within the 0.5 s budget
(modulo in-flight attempt finish). Pending jobs keep today's
memory-resident semantics (restart clears them — no
regression, durability is a recorded follow-up).

Preserved behavior: drain order, per-attempt semantics,
MusicBrainz 1/s pacing, quiet-shutdown path.

Intended changes: shutdown-signal check per drain iteration
(and per gate wait) in the identify service only; no schema
change; no wiring change beyond passing the existing shutdown
signal if not already reachable.

Acceptance tests: brief-first, minimal: SIGTERM mid-drain
exits 0 quickly with the drain abandoned (focused test only,
no 10k-job waits — seed a small drain and assert the loop
yields promptly); quiet shutdown still 0.03 s class.

Destructive ops: scratch state only; never prod.

Permitted external systems: none.

Non-goals: identify-queue durability; scan persistence (fix
A); query-shape perf (fix C).

Completion criteria: row-3 re-measurement passes (clean
shutdown mid-drain); review loop clean (implementer !=
reviewer).
