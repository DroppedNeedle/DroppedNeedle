# DroppedNeedle v3.0.0 readiness checklist

Release readiness is per-gate, not vibes: every stage below shipped only
with its review loop clean (zero blocker/major findings), and stage 13
re-verifies the load-bearing ones. Check each box with evidence linked,
not from memory.

Companion doc: `docs/v3-cutover-runbook.md` (rehearsed 2026-10-04).

## Stage gates 1-12 (all COMPLETE, review loops clean)

| Stage | Scope | Evidence |
|---|---|---|
| 1 | Skeleton, CI, Docker, contract pipeline, spikes, fixtures | `docs/v3-stages/stage-1.md` |
| 2 | Persistence, migrations, config/secrets core | `docs/v3-stages/stage-2.md` |
| 3 | Auth, users, sessions, app passwords | `docs/v3-stages/stage-3.md` |
| 4 | Native read APIs | `docs/v3-stages/stage-4.md` |
| 5 | Metadata providers + enrichment matrix | `docs/v3-stages/stage-5.md` |
| 6 | Remote sources + streaming gateway + playback reporting | `docs/v3-stages/stage-6.md` |
| 7 | Acquisition (requests, downloads, indexers, wanted, Lidarr) | `docs/v3-stages/stage-7.md` |
| 8 | Library engine (scan, identify, contributions) | `docs/v3-stages/stage-8.md` |
| 9 | Compat APIs (Subsonic + Jellyfin, contract-first) | `docs/v3-stages/stage-9.md` |
| 10 | Settings, admin, users, jobs, plugins, system, events | `docs/v3-stages/stage-10.md` |
| 11 | v2 to v3 export/import + dev tooling | `docs/v3-stages/stage-11.md` |
| 12 | Frontend migration to `/api/v3` | `docs/v3-stages/stage-12.md` |

- [ ] Each manifest above reads COMPLETE with zero open blocker/major
      findings (spot-check the manifest, not this table).

## Stage 13 gates

- [ ] Per-source release gates green (Q5: slskd, SABnzbd, Newznab, Lidarr
      import each gated alone; the OR-readiness check is smoke only).
      Evidence: `docs/v3-stages/stage-13-gates.md` sections 1-2
      (2026-10-04 re-run, all pass).
- [ ] Compat matrix re-run green. Evidence: same file, 225 passed.
- [ ] Crash/recovery suite re-run green. Evidence: same file, 83 passed.
- [ ] Cutover rehearsal green on a cloned instance (export, validate,
      dry-run, import, boot, smoke journeys, rollback, verify).
      Evidence: `docs/v3-cutover-runbook.md` appendix.
- [ ] Full-budget verification on reference hardware green. Confirm with
      the owning stage-13 worker; link the record here before release.
- [ ] Escape-hatch final evaluation recorded (all five, trip/no-trip with
      evidence). Confirm with the owning worker; link here.
- [ ] Rollback-boundary decision recorded [OWNER]: either sealed v3
      publication journals/snapshots stay usable after application
      rollback, or beta users explicitly accept that rollback does not
      reverse file mutations. Required before beta touches real
      libraries (Q9). Currently open.

## Owner-executed steps (in order)

These are exclusively owner actions. Agents must not do them.

- [ ] [OWNER] Record the rollback-boundary decision (above).
- [ ] [OWNER] Give `dev-image.yml` a v3 counterpart (it is still v2-shaped:
      triggers on `main`, builds the v2 Dockerfile). Required before any
      v3 `:dev` publish means something.
- [ ] [OWNER] Push the `v2-final` branch to origin (it exists locally only;
      see lineage note). Never as a `v*` tag.
- [ ] [OWNER] Run the real-instance cutover per `docs/v3-cutover-runbook.md`
      (steps 1-8), verify green.
- [ ] [OWNER] Publish the `:dev` image (runbook step 9).
- [ ] [OWNER] Replace `main` with `v3` when ready.
- [ ] [OWNER] Tag `v3.0.0`, which publishes the release images.

## v2-final lineage note

`v2-final` is a branch, deliberately never a `v*` tag (tags trigger image
publishes). It marks the v2 lineage at `cf7278a1`, the commit `v3` was cut
from. Local state 2026-10-04: `v2-final` resolves to
`cf7278a11454c2c5e7417e486b027dd98dfc9049` and is not on any remote, so
preserving it is one owner push. Rollback after release is the last v2
release image plus its own data dir (Q9).
