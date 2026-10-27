# Upgrading from v2 to v3

v3 is a rewrite with a new database. One command turns your v2 data into v3
data where it sits. You keep the same folders and the same compose mounts.

Your music files are not touched. Your library comes across as v2 knew it,
with the same ids, and v3 checks it against your files on its first scan.

## Upgrade

1. Stop v2:

   ```sh
   docker compose stop
   ```

   Let it stop on its own. A v2 that was killed can leave unsaved writes
   behind, and the upgrade refuses those (`V2_WAL_PRESENT` below).

2. Change the image tag in your compose file to v3. Leave the volumes as
   they are.

3. Run the upgrade:

   ```sh
   docker compose run --rm droppedneedle droppedneedle-tool upgrade
   ```

   `droppedneedle` is the service name in your compose file. Use yours if it
   differs.

4. Start v3:

   ```sh
   docker compose up -d
   ```

The upgrade prints what it is doing, then a summary: how many of each kind
of record moved, what stayed behind, anything worth a look (such as an
artist kept under its v2 id), which settings are back at their defaults,
and where your v2 files are now. The full report is saved as
`upgrade-report.json` in your cache folder.

Upgrade before anyone uses v3. If you start v3 on a v2 data directory by
mistake, it refuses to start and names this command. Nothing is changed.

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
The upgrade does need some free space in the cache folder while it runs,
about twice the size of the v2 database plus the originals Library
Management saved. It checks before it moves anything.

The backup sits on the same disk as your data. It lets you go back to v2,
but it is not a second copy. If you want one, copy your config and cache
folders somewhere else while v2 is stopped, before you upgrade.

Once you are happy with v3, delete the backup:

```sh
docker compose run --rm droppedneedle droppedneedle-tool remove-v2-backup
```

This works while v3 is running. To keep no backup in the first place, run
`droppedneedle-tool upgrade --no-backup`. The v2 files are then deleted as
soon as the import has finished, along with any files of your own in the
two folders, and there is no way back to v2 without a copy of your own.

### If the upgrade fails or is interrupted

If anything fails, the upgrade deletes what it had created, moves your v2
files back where they were, and tells you what went wrong. You are back on
v2 data and can try again once the cause is fixed. The table at the end of
this page lists the messages.

If the upgrade is killed partway (a power cut, a closed terminal), run the
same command again. It first puts the v2 files back, then starts over. v3
refuses to start until it has.

The upgrade refuses to run while a server is using the data directory, and
it does nothing on a data directory that is already on v3.

### Follows of deleted users

v2 could keep follows and auto-download approvals that belonged to users
who had since been deleted. The upgrade removes those rows and tells you how
many. Nothing else in the v2 database is changed.

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
- Your library: every album and song with the id v2 gave it. Most
  artists get a new id once, the one v3 finds them by when it scans, so
  each artist keeps its songs; the id v2 gave an artist still opens it.
  An artist that can't take that id (Various Artists, one with no songs,
  or one whose name another artist already holds) keeps its v2 id, and
  the summary names any kept apart by name.
  Playlists, favorites, history, queues and bookmarks find their songs and
  artists again. Subsonic apps like Symfonium or Feishin keep everything:
  song and album ids do not change, and an artist they know by its v2 id
  still opens. In Jellyfin apps like Finamp, stars,
  play counts and playlists are kept and downloaded music stays matched,
  but album and artist ids change once, so that nobody without an account
  can work one out. An album or artist marked to keep offline has to be
  marked again once; the music already on the device stays.
- Matches you made by hand (and v2's own matches), retired ids that still
  point at the album or artist that replaced them, "keep as tagged" and
  "exclude" decisions, edition pins and custom editions, albums you kept
  out of Library Management, and field overrides.
- The original of every file Library Management changed. "Restore original"
  in v3 puts such a file back where it was and gives it back the tags it had
  before v2 first touched it.
- Downloads held back for review, with their files and the reason
  they were held. They land in the `held` folder of your v3 cache and
  join the list of held files on the Downloads page, one entry per album.
  v2's "waiting on the library" holds read as a file already in the way;
  a reason v3 does not use reads as held by an earlier version. Held
  files and uploads waiting for you are kept at least 90 days after the
  upgrade.
- MusicBrainz contributions you had in progress, with their verification
  and the return link MusicBrainz calls when you save the release.
- YouTube links you found or set for albums and tracks.
- The releases each user hid from the discover queue.

Every edition you chose in v2 stays the album's edition in v3: a manual
match, an edition pin, or an active custom edition. A pin becomes your
choice of edition, exactly as if you had picked it in v3, and the files are
placed on that edition's tracks in the background once MusicBrainz is
reachable. v3's automatic identification never replaces it; only you can,
from the album page. If an album had both a manual match and a pin naming a
different release, the manual match wins and the import report says so.
Upgrading an existing v3 install does the same with any pins it holds.

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
- Requests still waiting for approval or downloading, and unfinished
  downloads. Ask again in v3.
- Download attempts still cleaning up their download folder.
- Sign-in sessions. Everyone signs in again.
- MusicBrainz contributions that were already linked, cancelled or out of
  date. A linked album keeps its match.
- Caches, which v3 fills again on its own.
- Plugins. Their settings come across, switched off, and the report lists
  each one as needing porting. Port it, install it again from Settings >
  Plugins, then switch it on.

## What changed from v2

Things v2 did that v3 does differently or no longer does. Read this before
you upgrade.

### Before you upgrade

- **Callback addresses.** v2 was called back on three `/api/v1` addresses.
  v3 still answers on all three, so nothing has to change with the
  upgrade:
  - Single sign-on (OIDC): the redirect URI `/api/v1/auth/oidc/callback`
    keeps working. To move to `/api/v3/auth/oidc/callback`, change it in
    your provider and under Settings > Security together.
    Check before you upgrade that at least one admin can also sign in with
    a username and password. If single sign-on breaks and nobody can sign
    in, see [Locked out](#locked-out) below.
  - Spotify playlist import: v3 goes on sending the redirect address v2
    registered, so the Spotify dashboard needs no change (see
    [After the upgrade](#after-the-upgrade)).
  - MusicBrainz contributions: a release editor tab opened from v2 returns
    to `/api/v1/library/contributions/musicbrainz/callback`, which v3 still
    answers. Contributions started in v3 use the v3 address.

### Locked out

If nobody can sign in to v3 (single sign-on fails and no admin remembers a
password), make a password recovery code on the host:

```sh
docker compose stop droppedneedle
docker compose run --rm droppedneedle droppedneedle-tool recovery-code USERNAME
docker compose up -d droppedneedle
```

`droppedneedle` is the service name in your compose file. Use yours if it
differs. Stop the server first: the command refuses to run while it is up. It
prints a one-time code. Open "Forgot password?" on the sign-in page and
enter the username, the code and a new password within 15 minutes.

`USERNAME` is the account's DroppedNeedle username, which can differ from
the name it has with your single sign-on provider. To see the admin
usernames (again with the server stopped):

```sh
docker compose run --rm droppedneedle droppedneedle-tool recovery-code --list-admins
```

An account that only ever signed in through single sign-on, Plex or
Jellyfin has no password yet. The command says so, and the code then
gives the account its first password; its other sign-in stays linked.
Only this command can do that. A recovery code an admin makes under
Settings > Users is for accounts that already have a password.

The command asks for no password of its own: anyone with access to the
machine and its data folders can run it.

### Removed

- **Free Music.** v2 had its own download client for Creative Commons and
  public-domain music from the Internet Archive. v3 has none. Its settings
  tab and its downloads list are gone. Music it already downloaded stays in
  your library. If Free Music was your only source, set up slskd, Usenet or
  a plugin source.
- **v2 plugins.** v2 plugins do not load in v3. Only plugins written for
  plugin API version 2 are accepted, and a manifest with `api_version` 0
  or 1 is refused. Port yours first ("Porting a v2 plugin" in
  [PLUGINS.md](../PLUGINS.md)), or ask its author to, then install it
  again from Settings > Plugins.
- **v2 profile share codes.** A Library Management profile share code made
  in v2 does not import into v3. Share the profile again from v3 to get a
  code v3 reads.
- **Wanted watches going dormant.** v2 stopped checking a watch once it was
  older than "Go dormant after (days)", 365 by default, until someone
  resumed it. v3 keeps checking a watch until the album arrives or someone
  stops it, and the setting is gone. A watch that was dormant starts
  watching again and is checked on the first pass. Stop the watches you no
  longer want.
- **"Restart under current policy".** v2 saved the quality policy each
  download started with and could restart a download under the policy in
  force now (`POST /api/v1/downloads/<id>/restart-with-current-policy`). v3
  saves no policy per download. It reads the current policy as it works, so
  a policy change already applies to what is waiting. Nothing to do.
- **Similar-songs modes for Subsonic and Jellyfin apps.** v2's Connect Apps
  settings offered three: local only, related artists from MusicBrainz, and
  Last.fm or ListenBrainz when linked. v3 always answers from your own
  library and has no setting. If you used one of the other two modes, the
  similar songs your apps show will change.
- **The staged "Match exact edition" job.** v2 downloaded a complete exact
  release as a job with its own steps: a space check, a download, a preview,
  then Apply, with retry, recheck and cancel along the way. In v3 you choose
  the edition on the album page and press one button to get the tracks it is
  missing. The album page shows what is being fetched and what failed, and
  lets you retry or stop it.
- **The AudioDB daily sweep.** v2 went through your library once a day
  fetching artist and album images from TheAudioDB, and queued what you
  browsed for the same. v3 fetches them after a library scan and when you
  press "Refresh library images" under Settings > Cache. An artist image is
  also fetched when a page first needs it. Nothing to do. A page you have
  never opened may take a moment longer to show its images.
- **Cache tab buttons and tiles.** v2's cache was split into memory, disk
  metadata and library parts, each with its own count and its own clear
  button. Those parts no longer exist. Settings > Cache in v3 counts cached
  provider answers and cached images, and clears the images or everything.
  Provider answers are kept in `metadata-cache.db` and survive a restart;
  saving a setting never clears them.
- **Settings nothing used, and the cache and pacing knobs.** These v2
  settings are gone. Most were never read; the cache lifetimes and the
  library image pacing now use fixed values equal to v2's defaults. An
  import of a v2 export carrying them goes ahead, and its report lists
  each one as `dropped_setting`.
  - Settings > Advanced: every server and browser cache lifetime
    (including TheAudioDB's), the memory and disk cache limits, the HTTP
    timeouts and pool size, the library image batch sizes, delays and
    workers, the image refresh's stall and run timeouts, the Discover
    queue's lifetime, card details cache and status polling interval, the
    genre section lifetime, request concurrency and the orphan cover
    cadence.
  - Settings > Download Client: slskd's own copies of the quality,
    verification, scoring, timeout, retry and concurrency settings (the
    download policy's are the ones that apply), and SABnzbd's client type
    and post-processing level.
  - Download policy: "Verify downloads", the Usenet release age before
    blocklisting, the source selection mode, the legacy quality order,
    lossy bounds and lossless preferences, the unknown-quality choice, and
    the target bitrate of a custom MP3 recipe step.
  - The server-wide scrobble targets (each user's own scrobble switches
    stay), the server-wide home page section settings (each user's own
    section choices stay), "Scrobble to Plex" (a Plex track
    that ends is always reported), Connect Apps' exact-track approval flag,
    MusicBrainz's concurrent searches, and the scan schedule's stored "last
    scan" (it now comes from the scan history).

  The outbound HTTP timeouts are environment variables, `HTTP_TIMEOUT` and
  `HTTP_CONNECT_TIMEOUT`, as before.
- **Max connections.** v2 had a "max connections" setting for outbound HTTP,
  also read from `HTTP_MAX_CONNECTIONS`. v3 has neither. The nearest is the
  `HTTP_MAX_KEEPALIVE` environment variable, the number of idle connections
  kept per host. Remove `HTTP_MAX_CONNECTIONS` from your compose file if you
  set it; v3 ignores it.
- **slskd concurrency variables.** v2 read
  `DOWNLOAD_CLIENT_CONCURRENT_SEARCHES` and
  `DOWNLOAD_CLIENT_CONCURRENT_ENQUEUES`, both 1 by default, to limit how
  hard it pushed slskd. v3 ignores them. It waits and tries again when slskd
  answers that it is busy. Remove them from your compose file if you set them.

### Changed

- **Reverse proxy headers.** v2 accepted a change from the web UI whatever
  address the page was on. v3 only accepts it from the address it believes
  it is served at, so your proxy has to tell it the truth. List the proxy
  in `TRUSTED_PROXY_IPS`, have it send `X-Forwarded-Proto`, and pass the
  `Host` header as the browser sent it, port included: in nginx,
  `proxy_set_header Host $http_host;` rather than `$host`. If saving
  anything in the web UI answers `403 Origin not allowed` after the
  upgrade, this is why, and the log names both addresses the first time.
  The fix is to pass the browser's `Host` (or send it as
  `X-Forwarded-Host`) and `X-Forwarded-Proto` through. If you cannot
  change the proxy, state the address under Settings > Security > Public
  address, or on the host with `droppedneedle-tool public-address`:
  [Reverse proxy and security](reverse-proxy-and-security.md#public-address)
  has the steps.
- **Content-security policy.** v2 sent none. v3 tells the browser what
  the web UI may load and run, and enforces it. You may notice it in two
  places: a dashboard that shows DroppedNeedle in a frame (below), and a
  reverse proxy that rewrites the page or injects a script, such as
  Cloudflare's Rocket Loader, which leaves a blank page whose browser
  console names the policy. The switch is under Settings > Security >
  Content-security policy, and on the host with `droppedneedle-tool
  content-security-policy` when the page is blank:
  [Reverse proxy and security](reverse-proxy-and-security.md#content-security-policy)
  has the steps.
- **Showing DroppedNeedle in a dashboard.** v2 could be shown in a frame
  by any page. v3 refuses that by default. If you open DroppedNeedle inside
  a dashboard such as Homarr or Organizr, add the dashboard's address under
  Settings > Security > Content-security policy > "Allowed to frame
  DroppedNeedle" after upgrading. A dashboard on another site (not a
  subdomain of the same domain, nor the same host on another port) shows
  the sign-in page even then, because the browser does not send the
  sign-in cookie to a frame there.
- **Performer tags.** v3 writes performer credits the way Picard does:
  "Name (instrument)", and an instrument list in ID3. The first time v3
  writes performer credits to an album v2 managed, it rewrites the ones v2
  wrote in that form. Like every managed change, the run can be undone.
- **MP3 tags that mix ID3v2.3 and v2.4.** An MP3 keeps its ID3 version,
  except an ID3v2.3 tag that also holds ID3v2.4 frames. Picard and iTunes
  write sort names that way, so most MP3s they tagged count as mixed. The
  first time v3 writes such a file it saves the whole tag as ID3v2.4, so
  every tag keeps its own frame; the preview says so for each file. There
  is no switch to keep ID3v2.3, and a few old players read only ID3v2.3.
  Undo and "Restore originals" put the values back; the tag stays ID3v2.4.

### Moved

- **Playlist "request missing" for scripts.** v2 had a server route,
  `POST /api/v1/playlists/<id>/request-missing`. The button on the playlist
  page works as before, but v3 has no such route: the page works out the
  missing albums itself and sends them to `POST /api/v3/requests/batches`.
  A script that called the old route has to do the same.

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

Then check:

- You can log in with your v2 password.
- Your Subsonic or Jellyfin app still connects with its app password. For
  the first moments after each start, while the server records its
  Jellyfin ids, Jellyfin apps are told the server is busy and try again.
- Settings > Download Client and Settings > Indexers / Prowlarr look right.
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
  report names any such folder. See
  [Import naming script](downloads.md#import-naming-script).
- If you import Spotify playlists: your Spotify app keeps working with no
  change in the Spotify dashboard. v3 goes on sending the redirect address
  v2 registered (Settings > Spotify shows it). Linked Spotify accounts carry
  over too, so nobody has to click Connect again.
- If you had a MusicBrainz contribution open in the release editor during
  the upgrade: saving it still brings you back, because v3 also answers on
  the v2 return address, and v3 links it, since the contribution came
  across with its return link.

## Going back to v2

As long as the `v2-backup` folders are there:

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

## Advanced: export and import by hand

The upgrade command runs an export, a check, a dry run and an import for
you. Each is also a command of its own, for moving to another machine, for
building the v3 data directory somewhere else, or for looking at the export
before you import it.

Paths below:

- `V2_ROOT`: your v2 data directory (it holds `config/` and `cache/`).
- `BACKUP_DIR`: a copy of `V2_ROOT` you take in step 1.
- `V3_ROOT`: a new, empty directory for v3.
- `WORK`: a scratch directory for the export file and reports.
- `V2_VERSION`: the v2 image tag or commit you ran, e.g. `v2.9.1`.

The examples run the tool with `docker run`, passing your own user and group
ids so the files it writes belong to you. Mount whatever directories each
command needs.

### 1. Stop v2 and take a copy

```sh
docker stop droppedneedle
cp -a V2_ROOT BACKUP_DIR
```

Never copy a running v2 database; stop it first.

### 2. Pick a passphrase

The export seals your secrets with a passphrase. Put it in a file only your
user can read:

```sh
printf '%s' 'a long passphrase' > WORK/passphrase.txt
chmod 600 WORK/passphrase.txt
```

The tool reads the passphrase from that file, never from the command line.

### 3. Export

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

### 4. Validate

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

### 5. Dry run

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

### 6. Import

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

Settings the export could not carry are listed under `settings_defaulted`
in `WORK/import.json`. Check that list for anything you expected to keep.

v3 keeps its settings in the database, beside the accounts, so the import
writes no `config.json`. `V3_ROOT/config` holds the key file
`data_enc.key`, which seals every stored password and API key.

### Backups and restore

A backup of the v3 database (the ones in `cache/backups`, or a copy that
`droppedneedle-tool restore` puts back) holds the settings as well. Keep
`config/data_enc.key` with it, and put it back beside a restored database:
without it, v3 refuses to start on that database rather than lose the
stored secrets. The database remembers which key it belongs to, so a key
file from another instance or another backup is refused as well.

The copies Undo and "Restore originals" need live in the `undo` folder
beside the database, and each backup copies the ones it names into
`cache/backups/undo`, which all kept backups share. Copy the whole
`backups` folder when you move backups elsewhere, not only the `.db`
files: `droppedneedle-tool restore` reads the copies from the `undo`
folder next to the backup and puts them back beside the restored
database. A copy that is missing there does not stop the restore; it is
listed, and Undo then leaves out only the file that needed it.

Backups leave out `metadata-cache.db` on purpose: it holds only provider
answers that are fetched again when needed.

If that key is lost for good, stop the server and run
`droppedneedle-tool reset-key --yes`. It gives the instance a new key and
clears every stored password and API key: the secrets in Settings, every
app password, and every user's link to Plex, Jellyfin, Navidrome, Spotify,
ListenBrainz and Last.fm. It prints the settings it cleared so you know
what to enter again. It replaces a lost key and does not change the
Jellyfin ids of what you already have: stored album and artist ids survive
the reset, so Finamp and Jellify keep their pins and downloads, and only
items added afterwards get ids under the new key. Whoever holds the old key
can still work out the picture ids of albums and artists stored before the
reset.
The replaced key is kept as `config/data_enc.key.old`. The command refuses,
changing nothing, while the key file still belongs to the database.

### 7. Start v3

Point your compose file's `/app/config` and `/app/cache` volumes at
`V3_ROOT/config` and `V3_ROOT/cache`, set `PUID`/`PGID` to the same ids you
used above, then start it. See
[docker-compose.example.yml](../docker-compose.example.yml).

```sh
docker compose up -d
curl -f http://localhost:8688/health
```

Then go through the checks under "After the upgrade" above.

### Each kind of data moves once

The import remembers, per v2 instance, which kinds of data it has brought
across: playlists, history, requests and so on. Running the same import
again changes nothing. Importing a newer export of the same v2 instance
later brings only the kinds that never came across before, so a playlist
you deleted in v3 does not come back. If you want a clean second try, start
again from an empty `V3_ROOT`.

Import before anyone signs in to v3. Someone who signs in first and, say,
uploads an avatar or links Last.fm keeps what they set up in v3, and the
import leaves their v2 version out.


## If something fails

| Message | Meaning | What to do |
|---|---|---|
| `this is a DroppedNeedle v2 data directory` | v3 was started on v2 data | Stop the container and run the upgrade |
| `an upgrade from v2 did not finish` | An upgrade was killed partway | Run the upgrade again, or `upgrade --undo --yes` to go back to v2 |
| `Stop the server, then try again` | A server is using the data directory | Stop it and re-run |
| `cannot tell what this data directory holds` | The folders are neither a v2 nor a v3 data directory, for example a v2 database beside a v3 key file | Nothing was changed. Check that the mounts point at the right folders |
| `already exists. Move it away or delete it` | A `v2-backup` folder is left from an earlier upgrade | Move or delete it, then run the upgrade again |
| `the upgrade needs about N MiB free` | Not enough free space in the cache folder | Free some space and run the upgrade again |
| `cannot move ...: Permission denied` | A file or folder in the config or cache folder could not be moved, because the user the tool runs as does not own it | Nothing is deleted. The upgrade normally puts the v2 files back itself; if it says that failed too, the next run does it. Fix the ownership (`chown -R` to your `PUID` and `PGID`) and run the upgrade again |
| `no complete v2 backup` | `--undo` found no `v2-backup` folders to go back to | Restore your own copy of the v2 folders |
| `V2_KEY_NOT_FOUND` | v2's `config/.env` is missing | Put it back from a copy of your own, then run again |
| `V2_KEY_MISMATCH` | The key in `config/.env` does not decrypt your stored secrets | Use the `config/.env` that v2 actually ran with |
| `V2_WAL_PRESENT` | v2's database still has writes in its `-wal` file | If v2 is still running, stop it first. Otherwise start v2 once and stop it cleanly, then run again |
| `DANGLING_USER_REF` | Orphan follows or approvals in v2 | The upgrade removes them itself. By hand: the step 4 cleanup, then export again |
| `INSTANCE_MISMATCH` | The export came from a different v2 instance | Export from the right `V2_ROOT` |
| `ENVELOPE_AUTH_FAILED` | Wrong passphrase | Re-run with the right passphrase file; no records were imported |
| `CHECKSUM_MISMATCH` | The export file was changed or truncated | Export again; do not edit the file |
| `BUNDLE_MISSING` | `export.bundle.sqlite` is not next to `export.json` | Put the two files in the same folder |
| `BUNDLE_MISMATCH` | The bundle next to the export comes from another export, or was changed | Use the bundle written together with that export, or export again |
| `BUNDLE_SECTION_MISSING` | The bundle lacks a part the export says it holds | Export again; do not edit the bundle |
| `BUNDLE_COLUMN_MISSING` | Your v2 is too old to have a value v3 needs for some data | Update v2 to its last release, start and stop it once, then upgrade again |
| `target database is locked` | A server has the v3 database open | Stop it and re-run |
| `the key file ... is missing` or `does not belong to the database` | The database's `data_enc.key` is not in the config folder | Nothing was changed. Put back the `data_enc.key` saved with the database, or set `CONFIG_DIR` to its folder. Only if it is lost for good: `reset-key --yes` (see "Backups and restore") |
| `a reset-key did not finish` | `reset-key` was stopped partway | Run `reset-key --yes` again to finish it |
