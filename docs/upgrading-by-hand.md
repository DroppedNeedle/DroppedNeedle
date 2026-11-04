# Upgrading from v2 by hand

The upgrade command runs an export, a check, a dry run and an import for
you (see [Upgrading from v2](upgrading-from-v2.md)). Each is also a command
of its own, for moving to another machine, for building the v3 data
directory somewhere else, or for looking at the export before you import it.

Paths below:

- `V2_ROOT`: your v2 data directory (it holds `config/` and `cache/`).
- `BACKUP_DIR`: a copy of `V2_ROOT` you take in step 1.
- `V3_ROOT`: a new, empty directory for v3.
- `WORK`: a scratch directory for the export file and reports.
- `V2_VERSION`: the v2 image tag or commit you ran, e.g. `v2.9.1`.

The examples run the tool with `docker run`, passing your own user and group
ids so the files it writes belong to you. Mount whatever directories each
command needs.

## 1. Stop v2 and take a copy

```sh
docker stop droppedneedle
cp -a V2_ROOT BACKUP_DIR
```

Never copy a running v2 database; stop it first.

## 2. Pick a passphrase

The export seals your secrets with a passphrase. Put it in a file only your
user can read:

```sh
printf '%s' 'a long passphrase' > WORK/passphrase.txt
chmod 600 WORK/passphrase.txt
```

The tool reads the passphrase from that file, never from the command line.

## 3. Export

```sh
docker run --rm -e PUID=$(id -u) -e PGID=$(id -g) \
  -v V2_ROOT:/v2:ro -v WORK:/work droppedneedle/droppedneedle:latest \
  droppedneedle-tool export --v2-root /v2 --out /work/export.json \
  --v2-commit V2_VERSION --passphrase-file /work/passphrase.txt
```

This writes two files: `export.json` and `export.bundle.sqlite` next to it.
The bundle holds the playlists, history and other user data; the JSON holds
the accounts, settings and every secret, sealed. Keep the two together.
Both are readable only by you, and both hold private data, so delete them
once you are done.

Like the upgrade, the export first lists the callback addresses that
change and stops with exit code 3 until you run it with
`--callbacks-changed` (see
[Upgrading from the command line](upgrading-from-v2.md#upgrading-from-the-command-line)). Pass
`-e BASE_PATH=...` if v3 runs under a base path, so the addresses it prints
are right.

If v2's key file (`config/.env`) is missing, or its key does not decrypt
your stored secrets, the export refuses rather than writing a broken file.
Restore `config/.env` from your backup and try again.

## 4. Validate

```sh
docker run --rm -e PUID=$(id -u) -e PGID=$(id -g) \
  -v V2_ROOT:/v2:ro -v WORK:/work droppedneedle/droppedneedle:latest \
  droppedneedle-tool validate /work/export.json --v2-root /v2
```

A clean file ends with `valid: N warning(s)`. Two warnings are normal:
`REVOKED_APP_PASSWORD_KEPT` (revoked app passwords stay revoked) and
`DANGLING_REVIEWER` (an approval whose reviewer no longer exists; the
reviewer field is cleared on import).

Above that it prints what stays behind in v2: one line per table with a row
count and the reason, then one per setting by name. Read it: this is the
moment to decide whether anything there matters to you before you move.

If it reports `DANGLING_USER_REF`, v2 has follows or approvals that point at
deleted users. Remove them from `V2_ROOT` (not the backup) and export again:

```sh
sqlite3 V2_ROOT/cache/library.db \
  "DELETE FROM user_followed_artists WHERE user_id NOT IN (SELECT id FROM auth_users);
   DELETE FROM auto_download_approvals WHERE user_id NOT IN (SELECT id FROM auth_users);"
```

## 5. Dry run

```sh
mkdir -p V3_ROOT/config V3_ROOT/cache
docker run --rm -e PUID=$(id -u) -e PGID=$(id -g) \
  -v V2_ROOT:/v2:ro -v V3_ROOT:/v3 -v WORK:/work droppedneedle/droppedneedle:latest \
  droppedneedle-tool dry-run --file /work/export.json \
  --db /v3/cache/library.db --config-dir /v3/config \
  --passphrase-file /work/passphrase.txt --v2-config /v2/config/config.json \
  > WORK/dryrun.json
```

It should exit 0, and `exit.code` in the report should be `OK` or
`OK_WITH_DROPS`. The report counts what would be imported per kind of
record. A dry run writes no records.

## 6. Import

Do this before v3 has ever run on `V3_ROOT`, so nobody has signed in to v3
yet (see "Each kind of data moves once" below). Use the same command with
`import` instead of `dry-run`:

```sh
docker run --rm -e PUID=$(id -u) -e PGID=$(id -g) \
  -v V2_ROOT:/v2:ro -v V3_ROOT:/v3 -v WORK:/work droppedneedle/droppedneedle:latest \
  droppedneedle-tool import --file /work/export.json \
  --db /v3/cache/library.db --config-dir /v3/config \
  --passphrase-file /work/passphrase.txt --v2-config /v2/config/config.json \
  > WORK/import.json
```

The counts match the dry run. Running the import again is safe: it changes
nothing the second time. If an import stops partway (a crash, a full disk),
run the same command again: everything already imported stays, and it picks
up at the first part it had not finished. The tool refuses to run while a
server has the target database open.

Profile pictures land in `V3_ROOT/cache/avatars`, the folder that holds the
database. If your v3 cache lives somewhere else, add
`--cache-dir /path/to/cache`.

`pending_links` in the report counts the song, album and artist ids the user
data named before the library itself came across. The report's
`library_link` notes say how many of them now point at your library and how
many name something v2 had already lost.

Settings v2 had nothing saved for are listed under `settings_defaulted` in
`WORK/import.json`; they start at v3's defaults. Settings v3 no longer has
are listed under `left_behind` and as `dropped_setting` notes. Check both
lists for anything you expected to keep.

v3 keeps its settings in the database, beside the accounts, so the import
writes no `config.json`. `V3_ROOT/config` holds the key file
`data_enc.key`, which seals every stored password and API key.

## Backups

A backup of the v3 database holds the settings as well. Keep
`config/data_enc.key` with it: [Backups and restore](backups-and-restore.md)
says what to keep and how to restore one.

## 7. Start v3

Point your compose file's `/app/config` and `/app/cache` volumes at
`V3_ROOT/config` and `V3_ROOT/cache`, set `PUID`/`PGID` to the same ids you
used above, then start it. See
[docker-compose.example.yml](../docker-compose.example.yml).

```sh
docker compose up -d
curl -f http://localhost:8688/health
```

Then go through the checks under
[After the upgrade](upgrading-from-v2.md#after-the-upgrade).

## Each kind of data moves once

The import remembers, per v2 instance, which kinds of data it has brought
across: playlists, history, requests and so on. Running the same import again
changes nothing. Importing a newer export of the same v2 instance later brings
only the kinds that never came across before, so a playlist you deleted in v3
does not come back. Its settings do replace v3's, so a setting you changed in
v3 goes back to the v2 value. If you want a clean second try, start again from
an empty `V3_ROOT`.

Import before anyone signs in to v3. Someone who signs in first and, say,
uploads an avatar or links Last.fm keeps what they set up in v3, and the
import leaves their v2 version out.
