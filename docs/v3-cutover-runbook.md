# DroppedNeedle v3 cutover runbook

Status: rehearsed 2026-10-04 on a scratch clone (export, validate, dry-run,
import, boot, smoke journeys, rollback, verify). Every command below ran as
written during that rehearsal; expected outputs are quoted from it.

Who does what: rehearsal is agent work and is done. Real-instance cutover,
the `:dev` image publish, pushing `v2-final`, replacing `main`, and tagging
`v3.0.0` are OWNER actions. Nothing in this file touches production until the
owner runs it there.

Terms used below:

- `V2_ROOT` - the live v2 data dir (holds `config/`, `cache/`, `plugins/`).
- `BACKUP_DIR` - a snapshot copy of `V2_ROOT`, taken before anything else.
- `V3_ROOT` - a fresh, empty v3 data dir. Never reuse the v2 dir in place.
- `WORK` - a scratch dir for the export file, reports, and passphrase file.

## 0. Preconditions

Check each one before starting. Cutover is a stop-the-world procedure: v2
goes down in step 1 and stays down until you either finish or roll back.

- [ ] v2 is healthy: container up, library loads, one login works.
- [ ] You know where `V2_ROOT` is and it holds `config/.env`
      (`DATA_ENC_KEY`), `config/config.json`, and `cache/library.db`.
- [ ] Enough disk for a full copy of `V2_ROOT` plus the export file
      (a few MB) plus the fresh v3 dir.
- [ ] `droppedneedle-tool` built from the `v3` branch. The v3 image does not
      ship it, so build it from a checkout:
      `cargo build --release --bin droppedneedle-tool`
      (binary lands at `server/target/release/droppedneedle-tool`).
- [ ] A v3 image or binary ready to boot (step 7), pointed at `V3_ROOT`.
- [ ] An operator passphrase picked and saved to a file readable only by
      you (`chmod 600`). It seals secrets inside the export. The passphrase
      never goes on a command line; the tool reads `--passphrase-file` or
      stdin.
- [ ] `python3` available (used for the SQLite checks below), or the
      `sqlite3` CLI with the same SQL.
- [ ] v2 commit SHA noted (`git rev-parse --short v2-final` on the host, or
      the running image tag). It goes into the export as provenance.

## 1. Stop v2 and snapshot the data dir

Stop the v2 container first. A live SQLite WAL must never be copied
file-wise, and the export must see a quiet database.

```sh
docker stop droppedneedle
cp -a V2_ROOT BACKUP_DIR
```

Sanity-check the snapshot: compare a checksum of `config.json` and run an
integrity check on a COPY of the database (never the live file while v2
could be running; v2 is stopped here, so the snapshot file is fine):

```sh
sha256sum V2_ROOT/config/config.json BACKUP_DIR/config/config.json
python3 -c "import sqlite3; print(sqlite3.connect('BACKUP_DIR/cache/library.db').execute('PRAGMA integrity_check').fetchone()[0])"
```

Expected: identical hashes, `ok`.

## 2. Export

```sh
droppedneedle-tool export --v2-root V2_ROOT --out WORK/export.json \
  --v2-commit <sha-from-step-0> --passphrase-file WORK/passphrase.txt
```

Expected output:

```text
wrote WORK/export.json
```

The exporter only reads v2. If the v2 key file is missing it refuses:

```text
V2_KEY_NOT_FOUND: v2 key file V2_ROOT/config/.env is missing; refusing to export
```

Fix: restore `config/.env` from `BACKUP_DIR` (or your secrets backup) and
re-run. Do not proceed without it; sealed secrets cannot be exported.

## 3. Validate

```sh
droppedneedle-tool validate WORK/export.json --v2-root V2_ROOT
```

A clean export prints its warnings, then:

```text
valid: 2 warning(s)
```

Two warnings are normal and benign: `REVOKED_APP_PASSWORD_KEPT` (revoked
passwords stay revoked by design) and `DANGLING_REVIEWER` (an approval
reviewer who is not an exported user; the field is nulled on import). The
`--v2-root` flag additionally proves the file came from THIS instance; a
file from anywhere else fails with `INSTANCE_MISMATCH`, exit 1.

If validation prints `error DANGLING_USER_REF ... invalid: N error(s)`,
go to step 4. For anything else, stop and read the Failure handling section.

## 4. Repair orphan rows (only if step 3 refused)

`DANGLING_USER_REF` means v2 holds follows or approvals pointing at users
that no longer exist. The importer fails closed on these; the fix is to
drop the orphan rows from v2 and re-export. v2 is already stopped, so this
is safe. Run against `V2_ROOT` (the snapshot in `BACKUP_DIR` stays
pristine for rollback):

```sql
DELETE FROM user_followed_artists WHERE user_id NOT IN (SELECT id FROM auth_users);
DELETE FROM auto_download_approvals WHERE user_id NOT IN (SELECT id FROM auth_users);
```

Via python3:

```sh
python3 -c "
import sqlite3
db = sqlite3.connect('V2_ROOT/cache/library.db')
f = db.execute('DELETE FROM user_followed_artists WHERE user_id NOT IN (SELECT id FROM auth_users)').rowcount
a = db.execute('DELETE FROM auto_download_approvals WHERE user_id NOT IN (SELECT id FROM auth_users)').rowcount
db.commit()
print('deleted follows', f, 'approvals', a)
"
```

Then repeat steps 2 and 3. Rehearsal deleted 1 follow + 1 approval and the
re-export validated clean.

## 5. Dry-run

Prepare the fresh target dir, then dry-run. `--v2-config` points at the v2
config so the one-shot `sync_frequency` carry (R8) is included in the
rehearsal exactly as in the real import.

```sh
mkdir -p V3_ROOT/config V3_ROOT/cache
droppedneedle-tool dry-run --file WORK/export.json \
  --db V3_ROOT/cache/library.db --config-dir V3_ROOT/config \
  --passphrase-file WORK/passphrase.txt --v2-config V2_ROOT/config/config.json \
  > WORK/dryrun.json
echo "exit=$?"
```

Expected: exit 0 and a report whose `exit.code` is `OK` or `OK_WITH_DROPS`
(`OK_WITH_DROPS` just records the benign warnings from step 3). Check the
per-entity counts match what v2 holds:

```sh
python3 -c "
import json
r = json.load(open('WORK/dryrun.json'))
print(r['exit'])
for e, c in r['entities'].items():
    nz = {k: v for k, v in c.items() if v}
    if nz: print(e, nz)
print('secrets_reencrypted:', r['secrets_reencrypted'])
"
```

Rehearsal printed `user {'imported': 2}`, `follow {'imported': 3}`,
`approval {'imported': 3, 'nulled_field': 1}`, `app_password
{'imported': 3}`, `provider {'imported': 3}`, `recovery_code
{'imported': 1}`, `secrets_reencrypted: 9`.

Dry-run writes nothing imported (no entities, no config.json), but it does
migrate the target schema first. That is by design; the real import reuses
the migrated file. If dry-run fails, see Failure handling. Do not import
until dry-run is green.

## 6. Import

Same command shape, `import` instead of `dry-run`, into the same target:

```sh
droppedneedle-tool import --file WORK/export.json \
  --db V3_ROOT/cache/library.db --config-dir V3_ROOT/config \
  --passphrase-file WORK/passphrase.txt --v2-config V2_ROOT/config/config.json \
  > WORK/import.json
echo "exit=$?"
```

Expected: exit 0, `exit.code` `OK`/`OK_WITH_DROPS`, and counts identical to
the dry-run (dry-run parity is a tested guarantee). The target now holds
`config/config.json` plus a fresh `config/data_enc.key`; secrets were
re-encrypted from the export seal into the v3 key.

Re-import is safe: running import again converges with zero new writes
(everything `skipped_identical`, exit 0), so a retry after a scare costs
nothing. The tool also refuses to import while a server holds the target
database (`target database is locked (stop the server before importing)`).

## 7. Boot v3

Point v3 at the imported dir. The server derives its paths from
`ROOT_APP_DIR`: `cache/library.db` for the database,
`config/config.json` for settings.

```sh
PORT=8688 ROOT_APP_DIR=V3_ROOT /path/to/droppedneedle
```

Or via compose (`docker-compose.v3.yml`, host port 18688 by default):

```sh
V3_PORT=18688 docker compose -f docker-compose.v3.yml up -d
```

Expected: `GET /health` returns 200 within seconds:

```sh
curl -o /dev/null -w 'health=%{http_code}\n' http://127.0.0.1:8688/health
```

Run exactly one v3 process. Durable-operation ownership is in-process and
breaks above one.

## 8. Verify

Run every check. All of them passed in rehearsal; quoted outputs are from
there (IDs and hashes will differ on a real instance).

Counts and users:

```sh
python3 -c "
import sqlite3
db = sqlite3.connect('V3_ROOT/cache/library.db')
for t in ['auth_users', 'user_followed_artists', 'auto_download_approvals', 'connect_app_passwords']:
    print(t, db.execute(f'SELECT count(*) FROM {t}').fetchone()[0])
print(db.execute('SELECT username, role FROM auth_users ORDER BY 1').fetchall())
"
```

Expected: counts equal the dry-run/import report; usernames and roles look
right.

Instance continuity:

```sh
python3 -c "
import json
v3 = json.load(open('V3_ROOT/config/config.json'))
v2 = json.load(open('V2_ROOT/config/config.json'))
print('match:', v3.get('instance_id') == v2.get('instance_id'), v3.get('instance_id'))
"
```

Expected: `match: True <id>`.

Login with a v2 password (proves hash import; no resets needed):

```sh
curl -s -c WORK/cookies.txt -H 'Content-Type: application/json' \
  -d '{"username":"<user>","password":"<v2-password>"}' \
  http://127.0.0.1:8688/api/v3/auth/login
```

Expected: 200 with `{"user":{...}}` and a `droppedneedle_session` cookie.
The first login rehashes the password to Argon2id; confirm with:

```sh
python3 -c "
import sqlite3
db = sqlite3.connect('V3_ROOT/cache/library.db')
print(db.execute(\"SELECT substr(provider_data, 1, 30) FROM auth_providers WHERE provider = 'local' LIMIT 1\").fetchone()[0])
"
```

Expected: `{"password_hash": "$argon2id$...`.

Authenticated read (reuse the cookie):

```sh
curl -s -b WORK/cookies.txt http://127.0.0.1:8688/api/v3/settings/connect-apps
```

Expected: 200 with the connect-apps JSON. Writes (PUT/POST) need an
`Origin:` header matching the server, or they return `FORBIDDEN`.

Compat re-auth with a surviving app password (needs Subsonic enabled in
settings; if you just enabled it, restart v3 once - the flag was observed
boot-time in rehearsal):

```sh
curl -s 'http://127.0.0.1:8688/subsonic/rest/ping?v=1.16.1&c=smoke&u=<user>&p=<app-password>'
```

Expected: `status="ok"`. A wrong or revoked secret returns code `40`
(`Wrong username or password.`), which proves secrets verify rather than
pass through.

Settings spot-check: open the settings UI (or query the settings
endpoints) and confirm indexers kept their order, download clients are
configured, and scan schedule reflects the old `sync_frequency`. Sections
the export did not carry are listed under `settings_defaulted` in
`WORK/import.json`; review that list for anything you expected to survive.

## 9. Publish the `:dev` image [OWNER]

Only after verification is green. Push the v3 `:dev` image so updaters and
testers move onto the migrated build. Concrete requirement: an image built
from `Dockerfile.v3` on this branch, tagged `:dev` and `:dev-<short-sha>`,
in `ghcr.io/droppedneedle/droppedneedle` and `droppedneedle/droppedneedle`.

Caveat, checked 2026-10-04: `.github/workflows/dev-image.yml` is still
v2-shaped (triggers on `main`, watches v2 paths, builds the v2
`Dockerfile`). It will NOT publish a v3 image as written. Before this step
can run on autopilot, the owner must give it a v3 counterpart that builds
`Dockerfile.v3`. Until then this step is a manual `docker buildx`
publish, or it waits.

## 10. Rollback

Use this if verification fails at any point, or any time after cutover
while v2 remains the fallback. Rollback restores software and data, with
one boundary (below).

1. Stop v3: `docker compose -f docker-compose.v3.yml down`
   (or stop the binary).
2. Restore the v2 data dir from the pre-cutover snapshot:
   `rm -rf V2_ROOT && cp -a BACKUP_DIR V2_ROOT`
3. Start the last v2 release image against `V2_ROOT`, as before.
4. Verify: container healthy, `PRAGMA integrity_check` returns `ok`,
   user/follow/approval counts match the snapshot, one login works.

Rehearsal restored a byte-identical `config.json` (matching sha256) and a
healthy database with pre-cutover counts.

Rollback boundary: restoring the v2 image + data dir restores software
and DB. It does NOT reverse files v3 renamed or retagged in the music
library. Before beta libraries are exposed to v3 publication, the owner
must either keep sealed v3 journals/snapshots usable after rollback, or
record that beta users accept the irreversibility (open decision - see
the readiness checklist). The cutover itself writes nothing into the
music library, so this boundary only matters once v3 starts publishing.

## Failure handling

| Symptom | Meaning | Fix |
|---|---|---|
| `V2_KEY_NOT_FOUND ... refusing to export` | v2 `config/.env` missing | Restore it from `BACKUP_DIR`; re-export. |
| `DANGLING_USER_REF`, `invalid: N error(s)` | Orphan follows/approvals in v2 | Step 4 repair, then re-export. |
| `INSTANCE_MISMATCH` | Export file is from another instance | Re-export from the right `V2_ROOT`. |
| `ENVELOPE_AUTH_FAILED: passphrase did not open sealed value at ...` | Wrong operator passphrase | Re-run with the right `--passphrase-file`. Zero writes happened (no config, no entities). |
| `target database is locked (stop the server before importing)` | A server holds the target DB | Stop v3, re-run. |
| Dry-run/import report `FAILED_VALIDATION` | Same as validate errors | Fix the cause, re-export, start again at step 5. |
| `status="failed" ... Subsonic API is disabled` on ping | Compat shim off in settings | Enable `subsonic_enabled` in settings, restart v3 once, re-ping. |
| Counts differ between dry-run and import | Should not happen (parity is tested) | Stop, keep both reports, investigate before booting. |

## Appendix: rehearsal record

- Date: 2026-10-04. Host scratch paths under `/tmp/cutover-rehearsal/`.
- Scratch v2 built by `tooling::fixture::build_v2_fixture` (2 users,
  4 follows, 4 approvals incl. 1 orphan each, 3 app passwords, Fernet +
  legacy-plaintext secrets, ordered indexers). Helper removed after use.
- Full loop proven: export, validate refusal on orphans, SQL repair,
  clean re-export with `--v2-commit cf7278a1`, validate clean (2 benign
  warnings), dry-run `OK_WITH_DROPS`, import with identical counts,
  boot on scratch port 18688, `/health` 200, alice login with v2 password
  + Argon2id rehash, settings read/write, Subsonic ping ok on the live
  app password and code 40 on wrong/revoked, idempotent re-import (zero
  new writes), rollback restore byte-identical with `integrity_check ok`.
- Prod untouched: the live `droppedneedle` container stayed up (healthy)
  throughout; no prod port or prod data was used.
