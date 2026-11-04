# Upgrading from v2 to v3

v3 is a rewrite with a new database. The upgrade turns your v2 data into v3
data where it sits. You keep the same folders and the same compose mounts.

Your music files are not touched. Your library comes across as v2 knew it.
Albums and songs keep their ids, and an artist's v2 id still opens it. v3
checks the library against your files on its first scan.

## Before you upgrade

- **Callback addresses.** v2 was called back on three `/api/v1` addresses.
  v3 answers none of them, so they stop working with the upgrade. The
  upgrade lists each one your v2 uses, with its new address, and waits
  until you say they are changed (see [Upgrade](#upgrade), step 6):
  - Single sign-on (OIDC): add the v3 redirect URI, ending in
    `/api/v3/auth/oidc/callback` instead of `/api/v1/auth/oidc/callback`,
    to the client at your provider. When v2's address ends in
    `/api/v1/auth/oidc/callback`, the upgrade changes it under Settings >
    Security for you; when it ends in a path of your reverse proxy's, the
    upgrade keeps it, so enter the new one there after the upgrade. Check
    before you upgrade that at least
    one admin can also sign in with a username and password. If single
    sign-on breaks and nobody can sign in, see [Locked out](#locked-out)
    below.
  - Spotify playlist import: add the v3 redirect URI, ending in
    `/api/v3/acquire/spotify/auth/callback`, to your app in the Spotify
    developer dashboard. Spotify takes only https addresses, or a
    loopback one such as `http://127.0.0.1`, so a plain-http address on
    your local network cannot be added.
  - MusicBrainz contributions: an edit open in MusicBrainz's release
    editor returns to v2's address, so it cannot come back to v3. Finish
    it in v2 first, or start it again from v3.

  Spotify and single sign-on providers such as Authentik, Authelia, Pocket
  ID, Google and Microsoft Entra ID let one client list more than one
  redirect address. Add the v3 address before you upgrade, and remove v2's
  once v3 signs in. If your provider allows only one, change it as you run
  the upgrade, not before.
- **`CONFIG_FILE_PATH`.** Remove it from your compose file if you set it:
  v3 refuses to start with it. Set `CONFIG_DIR` to that file's folder
  instead.
- **Unraid data folder.** v3 refuses to start with its database on a FUSE
  mount, and Unraid's `/mnt/user` paths are one unless the share is set to
  exclusive access. v2 ran there, so an existing install keeps that path.
  Before the v3 container first starts, either set the `appdata` share to
  exclusive access, or, with the container stopped, move the data folder's
  contents to a pool path such as `/mnt/cache/appdata/droppedneedle/cache`
  and point the container's `/app/cache` at it.
- **What changed.** [What changed in v3](v3-changes.md) lists what v2 did
  that v3 removed, changed or moved.

## Upgrade

1. Stop v2:

   ```sh
   docker compose stop
   ```

   Let it stop on its own. A v2 that was killed can leave unsaved writes
   behind, and the upgrade refuses those (`V2_WAL_PRESENT` below).

2. Change the image in your compose file to a v3 tag (such as `:v3.0.0`, or
   `:latest` once v3 is released). Leave the volumes as they are.

3. Start it:

   ```sh
   docker compose up -d
   ```

   On v2 data, v3 serves only the upgrade page. Until the upgrade is done,
   everything else answers that an upgrade is needed: the web app's API with
   `503 UPGRADE_REQUIRED`, and Subsonic and Jellyfin apps with their own
   error. Nothing in your data folders changes until you press Upgrade.

4. Open DroppedNeedle's address in your browser. It shows the upgrade page.

5. Sign in with the username and password of a v2 admin. See
   [Signing in](#signing-in) if no admin has a password.

6. Follow the page:
   - The pre-flight runs every check the upgrade runs before it moves
     anything, and changes nothing. It shows what it found, and anything to
     fix first, each with what to do. Fix it, then press Check again.
   - If v2 uses single sign-on, Spotify playlist import or MusicBrainz
     contributions, the addresses those services call back on change in v3,
     and v2's stop working (see [Callback addresses](#before-you-upgrade)).
     The page lists each one, old and new. Change it at the service, then
     tick it. Upgrade stays greyed out until every one is ticked.
   - Press Upgrade. The page lists the steps and shows each one as it runs.
     The upgrade carries on if you close the page; open it again to see where
     it is.
   - When it is done, the page shows a summary: how many of each kind of
     record moved, what stayed behind, anything worth a look (such as an
     artist kept under its v2 id), which settings are back at their defaults,
     and where your v2 files are now. The full report is saved as
     `upgrade-report.json` in your cache folder; the page shows its path.
   - Press Continue. The app starts in the same container, with no restart,
     and shows its sign-in. Sign in with your v2 password.

If your provider allows only one callback address, change it as you run the
upgrade, not before. When v2's single sign-on address ends in v2's path, the
upgrade changes it under Settings > Security for you; otherwise enter the new
one there after the upgrade. `upgrade-report.json` lists the addresses you
ticked under `acknowledged`. With single sign-on, the pre-flight lists it as
something to fix when no admin has a password or a username, since nobody
could get back in if single sign-on failed: set a username for an admin in
v2 (Profile) first.

### Signing in

The upgrade page has its own sign-in, separate from v3's. It takes the
username and password of an admin in v2. An admin who has no v2 password
(one who only ever signed in through single sign-on, Plex or Jellyfin) uses
a one-time code instead. Choose "Use a code from the server log" on the
page, and find the code in the server's log:

```sh
docker compose logs droppedneedle
```

It is on the line that contains `upgrade sign-in code`, after `code=` at the
end. `droppedneedle` is the service name in your compose file. Use yours if it
differs. A code works once: after it is used, the server prints a new one.

Attempts are limited per address and per username, as on v3's own sign-in.
The sign-in lasts 12 hours, or until the upgrade page closes.

### If the upgrade fails or is interrupted

If anything fails, the upgrade deletes what it had created, moves your v2
files back where they were, and the page says what went wrong. You are back
on v2 data. Fix the cause and press Try again, which runs the pre-flight
again. The table at the end of this page lists the messages.

If the upgrade is killed partway (a power cut, the container stopped), start
v3 again and open the page. It says "An upgrade didn't finish" and offers two
buttons:

- **Finish the upgrade** puts the v2 files back first, then upgrades again.
  If addresses still need changing, it stops once the v2 files are back.
  Press Try again to see them in the pre-flight.
- **Go back to v2** puts the v2 files back and stops there (see
  [Going back to v2](#going-back-to-v2)).

### Starting fresh

"Start fresh instead", beside Upgrade, brings nothing across. It asks first:
users, playlists, requests and history stay behind. It moves your v2 files
into the same `v2-backup` folders the upgrade uses, so going back to v2 still
works. Your music files are not touched. When you press Continue, v3 asks for
its first admin account. Then add your music folder under Settings > Library,
and v3 scans it from scratch.

Creating that account needs a code from the server log, so nobody who
reaches the page before you can take the server. It is on the line that
contains `first-admin setup code`, after `code=` at the end:

```sh
docker compose logs droppedneedle
```

The code is the one the upgrade page printed last. The server prints a new
one each time it starts, until the first account exists, so a restart never
locks you out.

### Where your v2 files go

The upgrade moves your v2 files into a `v2-backup` folder inside each of the
two data folders:

- `config/v2-backup/` holds v2's `.env` and `config.json`.
- `cache/v2-backup/` holds v2's `library.db` and the rest of the v2 cache.

Where the report or a message names `V2_ROOT/cache` or `V2_ROOT/config`,
it means these two folders.

Everything in the config and cache folders moves into the backup, not only
what v2 wrote. Files of your own that you kept there move too. Take out
anything you want to keep before you remove the backup.

Moving the files into the backup takes no time and no extra disk space.
The upgrade does need some free space in the cache folder while it runs:
about twice the size of the v2 database plus the files the upgrade carries
(saved originals, held downloads, profile pictures and playlist covers),
plus a little headroom. The pre-flight checks that.

The backup sits on the same disk as your data. It lets you go back to v2,
but it is not a second copy. If you want one, copy your config and cache
folders somewhere else while v2 is stopped, before you upgrade.

Once you are happy with v3, delete the backup:

```sh
docker compose run --rm droppedneedle droppedneedle-tool remove-v2-backup
```

This works while v3 is running. To keep no backup in the first place, upgrade
from the command line with `droppedneedle-tool upgrade --no-backup`. The v2
files are then deleted as soon as the import has finished, along with any
files of your own in the two folders, and there is no way back to v2 without
a copy of your own.

### Follows of deleted users

v2 could keep follows and auto-download approvals that belonged to users
who had since been deleted. The upgrade removes those rows and tells you how
many. Nothing else in the v2 database is changed.

## Upgrading from the command line

The page runs `droppedneedle-tool upgrade` for you. You can run it yourself
instead, with the server stopped:

1. Stop v2 and change the image tag, as in steps 1 and 2 above.

2. Run the upgrade:

   ```sh
   docker compose run --rm droppedneedle droppedneedle-tool upgrade
   ```

   If callback addresses change, the upgrade lists each one, old and new,
   and where to change it, and stops before it moves anything. If an earlier
   run was interrupted, it has put your v2 files back first. Change the
   addresses, then run it again with `--callbacks-changed`:

   ```sh
   docker compose run --rm droppedneedle droppedneedle-tool upgrade --callbacks-changed
   ```

   The flag skips no other check.

3. Start v3:

   ```sh
   docker compose up -d
   ```

The upgrade prints what it is doing, then the same summary the page shows.
If anything fails, it puts your v2 files back and says what went wrong. If it
is killed partway, run the same command again: it first puts the v2 files
back, then starts over. The upgrade refuses to run while a server is using
the data directory, and it does nothing on a data directory that is already
on v3.

### Checking first, starting fresh, scripted installs

`droppedneedle-tool upgrade --check` runs every check the upgrade runs
before it moves anything, counts what the export would carry, and changes
nothing in your data folders. It writes that export to the temporary folder
(`TMPDIR`) and deletes it again, so that folder needs the same free space
the upgrade asks of the cache folder. It exits 1 while something must be
fixed first, and 3 while the upgrade would stop on callback addresses or on
an admin nobody could get back in.

`droppedneedle-tool upgrade --fresh --yes` moves your v2 files into the
`v2-backup` folders as the upgrade does, and imports nothing. v3 then starts
empty and asks for its first admin account, with the code after `code=` on
the line that contains `first-admin setup code` in the server log (printed
again at each start until the first account exists). `upgrade --undo --yes`
goes back to v2 from there too.

For scripted installs, add `--progress json` to the upgrade or to any of
these. Each command then writes one JSON object a line to stdout: `step` and
`progress` lines, then one `result` or `failed` line. `--check` writes one
`check` line, or a `failed` line when it is called wrongly or cannot start.
The usual text goes to stderr.

## Locked out

If nobody can sign in to v3 (single sign-on fails and no admin remembers a
password), stop the server and make a one-time password recovery code on
the host with `droppedneedle-tool recovery-code USERNAME`.
[Forgotten passwords and recovery](accounts-and-sign-in.md#forgotten-passwords-and-recovery)
has the commands, and covers accounts that never had a password.

## What moves across

- User accounts and roles. Passwords keep working, no resets.
- App passwords for Subsonic and Jellyfin clients, and linked login
  providers. A password recovery code from v2 does not move across: it only
  lasts 15 minutes anyway, so make a new one if you need it.
- Settings, including saved API keys and secrets.
- Each user's linked Last.fm, ListenBrainz, Spotify, Navidrome, Jellyfin
  and Plex accounts. Scrobbling, Wrapped and Spotify playlist import keep
  working without linking again.
- Followed artists, auto-download approvals, the new-release feed and what
  each artist had already released (so v3 does not miss a release that
  came out around the upgrade).
- Playlists with their covers, favorites (with the names they had in v2)
  and play history. A playlist with no owner in v2 stays ownerless: public
  ones stay readable by everyone, and an admin may delete any of them. In
  v3, deleting a user deletes their playlists, favorites and follows.
- Finished requests and finished downloads, wanted watches, and the list of
  bad download sources.
- Per-user quota overrides. Past requests still count toward each user's
  request limit, and storage counts each finished download v2 recorded a
  size for.
- Concert cities and the concerts you marked as seen.
- Small per-user preferences: scrobble targets, home page sections,
  Navidrome folders, personal-mix approvals, and profile pictures.
- Saved play queues and bookmarks of Subsonic and Jellyfin apps.
- Your library: every album and song with the id v2 gave it. Most
  artists get a new id once, the one v3 finds them by when it scans, so
  each artist keeps its songs; the id v2 gave an artist still opens it.
  An artist that can't take that id (Various Artists, one with no songs,
  or one whose name another artist already holds) keeps its v2 id, and
  the summary names any kept apart by name.
  v2's own Unknown Artist becomes v3's: its songs, plays and favorites move
  there, and its v2 id still opens it. The summary says so. One with a real
  artist's id, a link to another service, or songs under another name stays
  apart, and the summary says why.
  Playlists, favorites, history, queues and bookmarks find their songs and
  artists again. Subsonic apps like Symfonium or Feishin keep everything:
  song and album ids do not change, and an artist they know by its v2 id
  still opens. In Jellyfin apps like Finamp, stars, play counts, playlists
  and downloads stay matched: every id v2 stored still opens its song,
  playlist, album or artist. Album and artist ids change once, so that nobody
  without an account can work one out, and the v2 id keeps working for anyone
  signed in. An album or artist marked to keep offline has to be marked again
  once; the music already on the device stays. A stored id that names nothing
  in v2's library any more, or that v3 already gives another item, is left out,
  and the import summary counts it.
- Matches you made by hand (and v2's own matches), retired ids that still
  point at the album or artist that replaced them, "keep as tagged" and
  "exclude" decisions, edition pins and custom editions, albums you kept
  out of Library Management, and field overrides.
- The original of every file Library Management changed. "Restore original
  state" in v3 puts such a file back where it was and gives it back the tags
  it had before v2 first touched it.
- Downloads held back for review, with their files and the reason
  they were held. They land in the `held` folder of your v3 cache and
  join the list of held files on the Downloads page, one entry per album
  (per song for a track download).
  v2's "waiting on the library" holds read as a file already in the way;
  a reason v3 does not use reads as held by an earlier version. Held
  files and uploads waiting for you are kept at least 90 days after the
  upgrade.
- MusicBrainz contributions you had in progress. v3 checks each one being
  verified again when it starts.
  An edit still open in MusicBrainz's release editor cannot come back to
  v3, since its return link names v2's address. Start it again from the
  album's contribution in v3; the upgrade's "Worth a look" list says how
  many there are.
- YouTube links you found or set for albums and tracks.
- The releases each user hid from the discover queue.

Every edition you chose in v2 stays the album's edition in v3: a manual
match, an edition pin, or an active custom edition. A pin becomes your
choice of edition, exactly as if you had picked it in v3, and the files are
placed on that edition's tracks in the background once MusicBrainz is
reachable. v3's automatic identification never replaces it; only you can,
from the album page. If an album had both a manual match and a pin naming a
different release, the manual match wins and the import report says so.

Albums that were waiting on a review question in v2 go back on v3's
identify queue, so v3 asks again with its own candidates.

Four passes run in the background after an upgrade: an album v2 matched only
to a release group is identified once more to find its edition, unless a
person chose that group; an album matched by itself on a release outside
the group all its files name is checked once more; an album matched by
itself on an edition whose barcode or catalog number differs from its
files', with no MusicBrainz release ID in them, is checked once more after
its files' edition tags are read; and the artist MusicBrainz links v2 made stay in
place until each album's release details arrive and are checked. See
[Your library](library.md#album-organization).

A song that points at something v2 itself no longer had (say a queued song
you deleted in v2) keeps its title and is left as it is.

Plays that name no song in the library (older ones, or all of them if you
scan your music again instead of carrying the v2 library) are matched to
your songs in the background after the first start and after each scan, and
then count in "recently played", "most played" and the play counts of your
apps. A play is matched only when it fits exactly one song; the rest stay in
your history by name.

## What stays behind

The upgrade prints everything it leaves behind with a count, and the saved
report lists it too (`left_behind`). In short:

- Undo of single Library Management changes made in v2 ("undo this
  change"). Restoring a file's original still works.
- Library Management job, preview and identification history, and scan
  state. v3 keeps its own from now on.
- Album art you picked by hand in v2. v3 reads album art from your files
  again.
- Held downloads you already imported or threw away.
- Playlists of users deleted in v2.
- Requests still waiting for approval or downloading, and unfinished
  downloads. Ask again in v3.
- Download attempts still cleaning up their download folder.
- Sign-in sessions. Everyone signs in again.
- MusicBrainz contributions that were already linked, cancelled or out of
  date. A linked album keeps its match.
- Caches, which v3 fills again on its own.
- Keys of v2's `config.json` that v3 no longer uses or reads from the
  environment instead, such as `port` and `contact_email`. Each is listed by
  name with what to do, and its value stays out of the list.
- Plugins. Their settings come across, switched off, and the report lists
  each one as needing porting. Port it, install it again from Settings >
  Plugins, then switch it on.

## Library Management originals

v3 never changes a file whose v2 original did not come across. If the
report lists an `original_baseline` as `dropped_invalid` (for example
because its saved tags were missing from v2's cache), v3 refuses to retag
or move that song. Otherwise its first change would record the file as v2
left it as the "original", and the real original would be lost. To fix it,
go back to v2 with `upgrade --undo --yes`, restore the
`library-management` folder in your cache from a copy of your own, and
upgrade again.

The upgrade copies these saved originals while it runs, so it needs more
free space if v2 managed a big library.

v3 recognises the files v2 tagged and goes on managing them. A file you
moved or renamed by hand after v2 last wrote it cannot be recognised, so
its album may show as left unmanaged after the upgrade. "Manage it again"
on the album page hands it back.

## After the upgrade

Run one v3 container per data directory, never two. A second server on the
same data folder refuses to start.

If your reverse proxy has a special rule for v2's live update stream at
`/api/v1/events/stream` (no buffering, long read timeout), move it to
`/api/v3/events/stream`. That's where v3 serves it. v2 also opened one
stream per download (`/api/v1/downloads/<id>/stream`); v3 sends download
progress over that same single stream, so a rule for those can go.

If you set your own contact address in v2, set it again in your compose
file. v3 reads it only from the environment. The upgrade's "Worth a look" list
says what to set, as `Set CONTACT_EMAIL=you@example.com in your environment`.
Without it v3 sends its default address to MusicBrainz. With the import by
hand, the same line is in `validate`'s `note:` lines and in the report's
`notes`.

Then check:

- You can log in with your v2 password.
- Your Subsonic or Jellyfin app still connects with its app password. For
  the first moments after each start, while the server records its
  Jellyfin ids, Jellyfin apps are told the server is busy and try again.
- Settings > Download Client and Settings > Indexers / Prowlarr look right.
- If you use Jellyfin, Settings > Jellyfin shows your address. An address v2
  kept only in its older top-level setting moves there, and "Worth a look"
  says so.
- Your library folders are listed under Settings > Library. They come across
  with your settings, so check them rather than adding them again: a folder
  added a second time gets a new id, and your carried library is filed under
  the old one. Mount your music at the same path v2 used. Then let the first
  scan run. Files that did not change since v2 last saw them are not even
  read again, and a song keeps its album while its tags still name it.
- Your v2 naming template becomes the import naming script under
  Settings > Library, rewritten so new downloads land where v2 put them,
  and the example under it shows where a track goes. Names are now cut at
  240 bytes rather than 252. A template with a folder made only of a value
  that can be missing, such as `{year}/`, now stops an import for an album
  without that value instead of leaving the folder out: the upgrade
  report names any such folder. A name ending in a dot keeps that dot when
  the template left out `.{ext}`, and a file name that is only dots stops
  the import. A template that can't be carried exactly
  falls back to the default script, and the report names it. See
  [Import naming script](downloads.md#import-naming-script).
- If you import Spotify playlists: the redirect address under Settings >
  Spotify is listed in your Spotify app in the developer dashboard. Linked
  Spotify accounts carry over, so nobody has to click Connect again.
- If you use single sign-on: you can sign in with it, and Settings >
  Security shows the v3 redirect address. Once it works, remove v2's
  address from your provider.

## Going back to v2

As long as the `v2-backup` folders are there.

Right after the upgrade, before you press Continue, the page's "Go back to
v2" does it: it asks first, deletes the v3 data and puts the v2 files back.
Then set the image tag back to your v2 version and start it again.

Later, from the command line:

1. Stop v3: `docker compose stop`.
2. Run the undo:

   ```sh
   docker compose run --rm droppedneedle droppedneedle-tool upgrade --undo --yes
   ```

3. Set the image tag back to your v2 version and start it.

The undo deletes everything in the config and cache folders except the
`v2-backup` folders, then puts the v2 files back where they were. Anything
done in v3 since the upgrade is lost, and so is any file of your own you
put in those folders after the upgrade. It asks for `--yes` because of
that. Without `--yes` it only says what it would do.

This restores the v2 database and settings. It does not undo changes v3
made to music files, such as renames or retags from Library Management.
Those only happen if you turned Library Management on and applied changes.

If you removed the backup, restore your own copy of the v2 config and cache
folders instead.

v2 answers only on its own callback addresses. If you changed them at your
single sign-on provider or in the Spotify dashboard and removed v2's, put
v2's back there.

## What changed in v3, and upgrading by hand

- [What changed in v3](v3-changes.md): what v2 did that v3 removed, changed
  or moved. Read it before you upgrade.
- [Upgrading from v2 by hand](upgrading-by-hand.md): the export, validate,
  dry run and import steps the upgrade runs for you, for moving to another
  machine or looking at the export first.

## If something fails

| Message | Meaning | What to do |
|---|---|---|
| `DroppedNeedle needs its upgrade from v2` (`503 UPGRADE_REQUIRED`) | v3 was started on v2 data, and an app or script called it | Open DroppedNeedle's address in a browser and run the upgrade there |
| `this is a DroppedNeedle v2 data directory` | A command that needs v3 data was run on v2 data | Upgrade first: start the server and open its page, or run the upgrade |
| `Before upgrading, change these addresses` (exit code 3) | v2 uses single sign-on, Spotify playlist import or MusicBrainz contributions, whose callback addresses change | Your v2 files are in place. Change each address it lists, then run again with `--callbacks-changed` |
| `No admin could get back in if single sign-on failed` (exit code 3) | Single sign-on is on and no admin has a password or a username | Your v2 files are in place. Set a username for the admin it names in v2 (Profile), then run again |
| `an upgrade from v2 did not finish` | An upgrade was killed partway | On the page, Finish the upgrade or Go back to v2. From the command line, run the upgrade again, or `upgrade --undo --yes` |
| `Stop the server, then try again` | A server is using the data directory | Stop it and re-run |
| `cannot tell what this data directory holds` | The folders are neither a v2 nor a v3 data directory, for example a v2 database beside a v3 key file | Nothing was changed. Check that the mounts point at the right folders |
| `already exists. Move it away or delete it` | A `v2-backup` folder is left from an earlier upgrade | Move or delete it, then run the upgrade again |
| `the upgrade needs about N MiB free` | Not enough free space in the cache folder | Free some space and run the upgrade again |
| `cannot move ...: Permission denied` | A file or folder in the config or cache folder could not be moved, because the user the tool runs as does not own it | Nothing is deleted. The upgrade normally puts the v2 files back itself; if it says that failed too, the next run does it. Fix the ownership (`chown -R` to your `PUID` and `PGID`) and run the upgrade again |
| `no complete v2 backup` | `--undo` found no `v2-backup` folders to go back to | Restore your own copy of the v2 folders |
| `V2_KEY_NOT_FOUND` | v2's `config/.env` is missing | Put it back from a copy of your own, then run again |
| `V2_KEY_MISMATCH` | The key in `config/.env` does not decrypt your stored secrets | Use the `config/.env` that v2 actually ran with |
| `V2_WAL_PRESENT` | v2's database still has writes in its `-wal` file | If v2 is still running, stop it first. Otherwise start v2 once and stop it cleanly, then run again |
| `DANGLING_USER_REF` | Orphan follows or approvals in v2 | The upgrade removes them itself. By hand: the cleanup in [Validate](upgrading-by-hand.md#4-validate), then export again |
| `INSTANCE_MISMATCH` | The export came from a different v2 instance | Export from the right `V2_ROOT` |
| `ENVELOPE_AUTH_FAILED` | Wrong passphrase | Re-run with the right passphrase file; no records were imported |
| `CHECKSUM_MISMATCH` | The export file was changed or truncated | Export again; do not edit the file |
| `BUNDLE_MISSING` | `export.bundle.sqlite` is not next to `export.json` | Put the two files in the same folder |
| `BUNDLE_MISMATCH` | The bundle next to the export comes from another export, or was changed | Use the bundle written together with that export, or export again |
| `BUNDLE_SECTION_MISSING` | The bundle lacks a part the export says it holds | Export again; do not edit the bundle |
| `BUNDLE_COLUMN_MISSING` | Your v2 is too old to have a value v3 needs for some data | Update v2 to its last release, start and stop it once, then upgrade again |
| `target database is locked` | A server has the v3 database open | Stop it and re-run |
| `is held by another process` | A server, or another import, has the v3 data open | Stop it and re-run |
| `the key file ... is missing` or `does not belong to the database` | The database's `data_enc.key` is not in the config folder | Nothing was changed. Put back the `data_enc.key` saved with the database, or set `CONFIG_DIR` to its folder. Only if it is lost for good: `reset-key --yes` (see [The key file](backups-and-restore.md#the-key-file)) |
| `a reset-key did not finish` | `reset-key` was stopped partway | Run `reset-key --yes` again to finish it |
