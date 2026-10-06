# Upgrading from v2 to v3

v3 is a rewrite with a new database. It does not open a v2 data directory
in place. You export from v2, import into a fresh v3 data directory, then
start v3 on that directory. Your v2 directory is never modified by the
export, so you can always go back.

Your music files are not touched. Your library comes across as v2 knew it,
with the same ids, and v3 checks it against your files on its first scan.

## What moves across

- User accounts and roles. Passwords keep working, no resets.
- App passwords for Subsonic and Jellyfin clients, linked login providers
  and recovery codes.
- Settings, including saved API keys and secrets.
- Each user's linked Last.fm, ListenBrainz, Spotify, Navidrome, Jellyfin
  and Plex accounts. Scrobbling, Wrapped and Spotify playlist import keep
  working without linking again.
- Followed artists, auto-download approvals, the new-release feed and what
  each artist had already released (so v3 does not miss a release that
  came out around the upgrade).
- Playlists with their covers, favorites (with the names they had in v2)
  and play history.
- Finished requests and finished downloads, wanted watches, and the list of
  bad download sources.
- Per-user quota overrides. Past requests still count toward each user's
  request limit, and storage counts each finished download v2 recorded a
  size for.
- Concert cities and the concerts you marked as seen.
- Small per-user preferences: scrobble targets, home page sections,
  Navidrome folders, personal-mix approvals, and profile pictures.
- Saved play queues and bookmarks of Subsonic and Jellyfin apps.
- Your library: every artist, album and song with the id v2 gave it. Apps
  like Symfonium, Feishin or Finamp keep their stars, downloads and queues,
  because the ids they know do not change. Playlists, favorites, history,
  queues and bookmarks find their songs again.
- Matches you made by hand (and v2's own matches), retired ids that still
  point at the album or artist that replaced them, "keep as tagged" and
  "exclude" decisions, edition pins and custom editions, albums you kept
  out of Library Management, and field overrides.
- The original of every file Library Management changed. "Restore original"
  in v3 puts such a file back where it was and gives it back the tags it had
  before v2 first touched it.
- Downloads held back for review, with their files. They land in
  `V3_ROOT/cache/held`.
- MusicBrainz contributions you had in progress, with their verification
  and the return link MusicBrainz calls when you save the release.

Every edition you chose in v2 stays the album's edition in v3: a manual
match, an edition pin, or an active custom edition. v3's automatic
identification never replaces it; only you can, from the album page. If an
album had both a manual match and a pin naming a different release, the
manual match wins and the import report says so.

Albums that were waiting on a review question in v2 go back on v3's
identify queue, so v3 asks again with its own candidates.

A song that points at something v2 itself no longer had (say a queued song
you deleted in v2) keeps its title and is left as it is.

## What stays behind

The export lists everything it leaves behind with a count, and so do
`validate` and the import report (`left_behind`). In short:

- Undo of single Library Management changes made in v2 ("undo this
  change"). Restoring a file's original still works.
- Library Management job, preview and identification history, and scan
  state. v3 keeps its own from now on.
- Album art you picked by hand in v2. v3 reads album art from your files
  again.
- Held downloads you already imported or threw away.
- Requests still waiting for approval or downloading, and unfinished
  downloads. Ask again in v3.
- Download attempts still cleaning up their download folder.
- Sign-in sessions. Everyone signs in again.
- MusicBrainz contributions that were already linked, cancelled or out of
  date. A linked album keeps its match.
- YouTube links and the discover queue's ignore list. v3 has nowhere to
  keep these yet; add them again in v3.
- Caches, which v3 fills again on its own.
- Plugins. Install them again from Settings > Plugins; their settings come
  across.

## Library Management originals

v3 never changes a file whose v2 original did not come across. If the
import report lists an `original_baseline` as `dropped_invalid` (for
example because its saved tags were missing from `V2_ROOT/cache`), v3
refuses to retag or move that song. Otherwise its first change would record
the file as v2 left it as the "original", and the real original would be
lost. Restore `V2_ROOT/cache/library-management` from your backup, export
again and import again into a fresh `V3_ROOT`.

The export copies these saved originals into the bundle, so the bundle can
be large if v2 managed a big library.

## Each kind of data moves once

The import remembers, per v2 instance, which kinds of data it has brought
across: playlists, history, requests and so on. Running the same import
again changes nothing. Importing a newer export of the same v2 instance
later brings only the kinds that never came across before, so a playlist
you deleted in v3 does not come back. If you want a clean second try, start
again from an empty `V3_ROOT`.

Import before anyone signs in to v3. Someone who signs in first and, say,
uploads an avatar or links Last.fm keeps what they set up in v3, and the
import leaves their v2 version out.

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

This writes two files: `export.json` and `export.bundle.sqlite` next to it.
The bundle holds the playlists, history and other user data; the JSON holds
the accounts, settings and every secret, sealed. Keep the two together.
Both are readable only by you, and both hold private data, so delete them
once you are done.

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

Above that it prints what stays behind in v2, one line per table with a
row count and the reason. Read it: this is the moment to decide whether
anything there matters to you before you move.

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
yet (see "Each kind of data moves once" above). Use the same command with
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

If your reverse proxy has a special rule for v2's live update stream at
`/api/v1/events/stream` (no buffering, long read timeout), move it to
`/api/v3/events/stream`. That's where v3 serves it.

Then check:

- You can log in with your v2 password.
- Your Subsonic or Jellyfin app still connects with its app password.
- Settings > Download Client and Settings > Indexers / Prowlarr look right.
- Your library folders are listed under Settings > Library. They come across
  with your settings, so check them rather than adding them again: a folder
  added a second time gets a new id, and your carried library is filed under
  the old one. Mount your music at the same path v2 used. Then let the first
  scan run. Files that did not change since v2 last saw them are not even
  read again, and a song keeps its album while its tags still name it.
- If you import Spotify playlists: your Spotify app keeps working with no
  change in the Spotify dashboard. v3 goes on sending the redirect address
  v2 registered (Settings > Spotify shows it). Linked Spotify accounts carry
  over too, so nobody has to click Connect again.
- If you had a MusicBrainz contribution open in the release editor during
  the upgrade: saving it still brings you back, because v3 also answers on
  the v2 return address, and v3 links it, since the contribution came
  across with its return link.

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
| `BUNDLE_MISSING` | `export.bundle.sqlite` is not next to `export.json` | Put the two files in the same folder |
| `BUNDLE_MISMATCH` | The bundle next to the export comes from another export, or was changed | Use the bundle written together with that export, or export again |
| `BUNDLE_SECTION_MISSING` | The bundle lacks a part the export says it holds | Export again; do not edit the bundle |
| `BUNDLE_COLUMN_MISSING` | Your v2 is too old to have a value v3 needs for some data | Update v2 to its last release, start and stop it once, then export again |
| `target database is locked` | A server has the v3 database open | Stop it and re-run |
