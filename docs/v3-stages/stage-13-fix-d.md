# Stage 13 fix step D — shutdown stops in-flight scans; watcher ignores own sidecar

Status: ACCEPTED 2026-10-04. Branch `v3`. Gate source:
`docs/v3-stages/stage-13-budgets.md` re-verdicts row 3 (FAIL,
new mechanism F4).

## Manifest

Scope: shutdown waits out in-flight scans (measured 17.3 /
22.4 s at 100k, exit 0) because nothing in the shutdown path
requests a scan stop (`stop_requested_at` stays NULL). Add
scan abort/stop-request on shutdown so SIGTERM during a scan
exits within the 0.5 s budget modulo in-flight file finish
(same bar as fix B's drain abort). Also fix the watcher
self-trigger: the server writes its own `publish.db` sidecar
inside watched roots, which queues a full rescan — own
sidecar writes must not retrigger the watcher (exclude the
`.droppedneedle-management-meta` dir and any server-owned
write paths from watch events).

Preserved behavior: scan semantics, verdicts, and results
when NOT shutting down; aborted scans resume/re-run cleanly
next start (no half-committed catalog state visible to
reads — reuse the existing transaction boundaries); quiet
shutdown still instant.

Intended changes: shutdown plumbing (scan stop-request +
abort checks in the walk/index loop), watcher exclusion for
server-owned paths; no schema change unless the stop signal
needs durable state (prefer in-process signal like fix B);
no reads/identify/migration changes.

Acceptance tests: brief-first, minimal: SIGTERM mid-scan
exits 0 quickly with the scan abandoned and the catalog
consistent (focused test, small corpus); sidecar write does
not queue a rescan (focused watcher brief).

Destructive ops: scratch state only; never prod.

Permitted external systems: none.

Non-goals: scan throughput + RSS (fix E); query perf (fix C,
done); identify drain (fix B, done).

Completion criteria: row-3 re-measurement passes mid-scan;
review loop clean (implementer != reviewer).
