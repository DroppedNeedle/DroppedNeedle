# Upgrading from v2 to v3

v3 is a rewrite with a new database. It does not open a v2 data directory
in place. You export from v2, import into a fresh v3 data directory, then
start v3 on that directory. Your v2 directory is never modified by the
export, so you can always go back.

What moves across: user accounts (passwords keep working, no resets),
roles, app passwords for Subsonic and Jellyfin clients, linked login
providers, recovery codes, followed artists, auto-download approvals, and
settings, including saved API keys and secrets. Your music files are not
touched.

What does not move: playlists, favorites, play history, per-user quota
overrides and per-user remote-server connections. Re-create those after the
upgrade. The library is rescanned from disk on first start.

Paths below:

- `V2_ROOT`: your v2 data directory (it holds `config/` and `cache/`).
- `BACKUP_DIR`: a copy of `V2_ROOT` you take in step 1.
- `V3_ROOT`: a new, empty directory for v3.
- `WORK`: a scratch directory for the export file and reports.
- `V2_VERSION`: the v2 image tag or commit you ran, e.g. `v2.9.1`.

The upgrade tool, `droppedneedle-tool`, ships in the v3 image. The examples
run it with `docker run`, passing your own user and group ids so the files it
writes belong to you. Mount whatever directories each command needs.

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

The same command with `import` instead of `dry-run`:

```sh
docker run --rm -e PUID=$(id -u) -e PGID=$(id -g) \
  -v V2_ROOT:/v2:ro -v V3_ROOT:/v3 -v WORK:/work droppedneedle/droppedneedle:latest \
  droppedneedle-tool import --file /work/export.json \
  --db /v3/cache/library.db --config-dir /v3/config \
  --passphrase-file /work/passphrase.txt --v2-config /v2/config/config.json \
  > WORK/import.json
```

The counts match the dry run. Running the import again is safe: it changes
nothing the second time. The tool refuses to run while a server has the
target database open.

Settings the export could not carry are listed under `settings_defaulted`
in `WORK/import.json`. Check that list for anything you expected to keep.

## 7. Start v3

Point your compose file's `/app/config` and `/app/cache` volumes at
`V3_ROOT/config` and `V3_ROOT/cache`, set `PUID`/`PGID` to the same ids you
used above, then start it. See
[docker-compose.example.yml](../docker-compose.example.yml).

```sh
docker compose up -d
curl -f http://localhost:8688/health
```

Run one v3 container per data directory, never two.

Then check:

- You can log in with your v2 password.
- Your Subsonic or Jellyfin app still connects with its app password.
- Settings > Download Client and Settings > Indexers / Prowlarr look right.
- Add your library path under Settings > Library and let the first scan run.
- If you import Spotify playlists: v3 sends Spotify a new redirect URI,
  shown under Settings > Spotify. Add it to your app in the Spotify
  developer dashboard next to the old one, or Spotify refuses the sign-in.
  Then each user connects Spotify again from their profile.

## Going back to v2

1. Stop v3.
2. Restore the v2 directory: `rm -rf V2_ROOT && cp -a BACKUP_DIR V2_ROOT`.
3. Start your last v2 image against `V2_ROOT`.

This restores the v2 software and database. It does not undo changes v3
made to music files, such as renames or retags from Library Management.
Those only happen if you turned Library Management on and applied changes.

## If something fails

| Message | Meaning | What to do |
|---|---|---|
| `V2_KEY_NOT_FOUND` | v2's `config/.env` is missing | Restore it from the backup, export again |
| `V2_KEY_MISMATCH` | The key in `config/.env` does not decrypt your stored secrets | Use the `config/.env` that v2 actually ran with |
| `V2_WAL_PRESENT` | v2's database still has writes in its `-wal` file | Start v2 once and stop it cleanly, then export again |
| `DANGLING_USER_REF` | Orphan follows or approvals in v2 | Step 4 cleanup, export again |
| `INSTANCE_MISMATCH` | The export came from a different v2 instance | Export from the right `V2_ROOT` |
| `ENVELOPE_AUTH_FAILED` | Wrong passphrase | Re-run with the right passphrase file; no records were imported |
| `CHECKSUM_MISMATCH` | The export file was changed or truncated | Export again; do not edit the file |
| `target database is locked` | A server has the v3 database open | Stop it and re-run |
