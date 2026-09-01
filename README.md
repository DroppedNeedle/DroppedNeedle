<div align="center">

<img src="Images/logo_wide.png" alt="DroppedNeedle" width="400" />

[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)
[![GitHub Stars](https://img.shields.io/github/stars/DroppedNeedle/DroppedNeedle?label=stars&logo=github&logoColor=white)](https://github.com/DroppedNeedle/DroppedNeedle)
[![Docker Hub](https://img.shields.io/badge/docker-hub-blue?logo=docker&logoColor=white)](https://hub.docker.com/r/droppedneedle/droppedneedle)
[![Discord](https://img.shields.io/discord/1356702267809808404?label=discord&logo=discord&logoColor=white)](https://discord.gg/B5suDg7gu2)
<br>

[![Docs](https://img.shields.io/badge/docs-droppedneedle.com-blue)](https://www.droppedneedle.com/)
[![GitHub Sponsors](https://img.shields.io/github/sponsors/HabiRabbu?label=sponsors&logo=github&logoColor=white)](https://github.com/sponsors/HabiRabbu)
<br>

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/M4M41URGJO)
<a href="https://github.com/sponsors/HabiRabbu"><img src="https://img.shields.io/badge/Sponsor%20this%20project-ea4aaa?style=for-the-badge&logo=github&logoColor=white" alt="Sponsor this project" style="border-radius: 6px; height: 30px" /></a>

</div>

---

DroppedNeedle is a self-hosted music request and discovery app with a **built-in native library and download engine** (no Lidarr required). Search the full MusicBrainz catalogue, request whole albums or single tracks, and let the engine index your library while it drives downloads through your own slskd or Usenet/SABnzbd. Its optional Library Management system can write tags and organize files after an administrator previews and enables it. Stream from Jellyfin, Navidrome, Plex, or your local files, get recommendations from your listening history, and scrobble to ListenBrainz and Last.fm. Play your library in third-party apps like Symfonium and Finamp over the OpenSubsonic and Jellyfin APIs. It all runs as a single Docker container, configured from the web UI.

---

## Screenshots

<img src="Images/Home.webp" alt="Home page with trending artists, popular albums, and personalized recommendations" width="100%" />
<img src="Images/Discover.webp" alt="Discover page with personalized album recommendations" width="100%" />
<img src="Images/Library.webp" alt="Library overview with statistics and recent additions" width="100%" />
<img src="Images/ListeningRoom.webp" alt="Listening Room - local files library with format and storage stats" width="100%" />
<img src="Images/Jellyfin.webp" alt="Jellyfin library view" width="100%" />
<img src="Images/Settings.webp" alt="Settings" width="100%" />

---

## Quick start

You need Docker, a music library, and a download client. The example below uses slskd; [SABnzbd](https://sabnzbd.org/) with Newznab indexers works too. DroppedNeedle does not ship or run either for you - see [slskd setup](#slskd-setup) and [Usenet setup](#usenet-setup).

> **DroppedNeedle only orchestrates a user-provided download client over its local HTTP API; it never joins or distributes on the Soulseek/P2P network. You supply, run, and are responsible for your own download client and, for slskd, its shared folders.**

### 1. Create a docker-compose.yml

Images are available on [Docker Hub](https://hub.docker.com/r/droppedneedle/droppedneedle) (`droppedneedle/droppedneedle:latest`).

```yaml
services:
  droppedneedle:
    image: droppedneedle/droppedneedle:latest
    container_name: droppedneedle
    environment:
      - PUID=1000            # Run `id` on your host to find your user/group ID
      - PGID=1000
      - UMASK=027            # Secure default; use 002 for trusted group-writable media
      - PORT=8688
      - TZ=Etc/UTC           # Your timezone, e.g. Europe/London, America/New_York
      - SLSKD_DOWNLOADS_PATH=/data/slskd/complete
    ports:
      - "8688:8688"
    volumes:
      - ./config:/app/config  # Persistent app configuration
      - ./cache:/app/cache    # Cover art and metadata cache
      - ./plugins:/app/plugins  # Installed plugins (omit and they vanish on recreate)
      - /path/to/media/imports:/app/imports  # Optional persistent drop-import staging
      # One common-parent mount enables fast moves between /data/slskd/complete and
      # /data/music. Configure /data/music as the library root. Do not add nested binds
      # beneath /data: each bind is a separate rename boundary even when it is on one disk.
      - /path/to/media:/data:rw
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8688/health"]
      interval: 30s
      timeout: 10s
      start_period: 10m
      retries: 3
```

> A `:dev` tag (`droppedneedle/droppedneedle:dev`) is also available. It's built automatically from `main` on every push and may be unstable. Pin to a specific commit with `:dev-<short-sha>` (e.g. `:dev-a1b2c3d`).

### 2. Start it

```bash
docker compose up -d
```

### Updating

Update DroppedNeedle the same way as any other container:

```bash
docker compose pull
docker compose up -d
```

The first start of a version with library changes may take longer. DroppedNeedle keeps
the health endpoint available and saves the database and settings under
`/app/cache/upgrade-backups`. It upgrades a working copy, checks the result, puts the
checked files into place, and validates the new app before any scanner, download worker,
or other background task starts. It does not rewrite or move music files. Fresh
installations create the new database directly, and later restarts skip completed
upgrades.

If the copied migration or pre-worker startup validation fails, DroppedNeedle restores
the database and settings from the backup and does not start against a partial upgrade.
Once validation is recorded and background work is allowed to start, automatic rollback
ends because newer catalog or settings changes may exist. Check the container log before
trying a newer image. Restoring the retained backup after that point can discard those
newer changes.

On startup after an upgrade, DroppedNeedle normalizes the saved MusicBrainz source. Missing
settings, legacy Official settings, malformed settings, and explicit Official settings migrate to
built-in BrainzMash. A valid self-hosted mirror or Community / external source stays selected.

Review **Settings > MusicBrainz** and its disclosure after an upgrade. An administrator can
explicitly switch to Official, a self-hosted mirror, or a Community / external source afterward,
and a deliberate Official save persists across restarts. This startup normalization is the
exception to the normal rule that source changes are explicit; runtime failures never trigger a hidden
fallback.

A previously upgraded installation will refuse to start if its database is later
missing, empty, or incomplete. Restore a verified backup rather than treating that state
as a fresh install.

Keep the image's default command and entrypoint; overriding either bypasses the startup
upgrade and is unsupported. The `/app/config` and `/app/cache` mounts must be writable and
support SQLite WAL locking, `fsync`, and atomic file replacement. This includes ordinary
Docker bind mounts and named volumes, plus local Unraid shares and TrueNAS datasets with
the usual container permissions. NFS, SMB, and other network mounts are safe only when
they provide those SQLite filesystem guarantees. On Docker Desktop for Windows, prefer
named Docker volumes over Windows-path bind mounts for `/app/config` and `/app/cache`;
the Windows mount translation does not reliably honor atomic file replacement. The
startup upgrade detects this and falls back to a verified direct copy, but named
volumes remain the recommended setup on Windows.

### 3. First-run setup

Open [http://localhost:8688](http://localhost:8688). On first launch you'll be prompted to create the first admin account (a username and password; email is optional); this only happens once. After that, add your library path under Settings > Library, add your slskd URL and API key under Settings > Download Client, then connect whichever streaming and discovery services you use. Run a library scan from Settings > Library.

---

## Native engine

DroppedNeedle replaces Lidarr with a built-in library and download engine. Library Scanning reads files, identifies them, and updates the catalog without changing the music on disk. Finished downloads are scored, verified, and published into the library through a staged writer. Library Management adds optional Picard-style tag, artwork, naming, and organization rules for existing files and future imports. It remains off until an administrator enables it. Lidarr is not used for library management.

> **The slskd downloads path is required for imports.** DroppedNeedle must be able to see it read-write. A common container mount with the library enables a fast atomic move; separate mounts use a safe copy-and-remove fallback and briefly need space for both copies. See [slskd setup](#slskd-setup).

### Legality boundary

The engine talks only to a user-supplied slskd instance over its local HTTP API. It never joins or distributes on the Soulseek/P2P network itself; it issues searches and download requests to slskd and imports the results. The operator supplies, runs, and is responsible for slskd and its shared folders. This is built into the architecture, not just the UI: the engine has no Soulseek protocol code, only an HTTP client for slskd.

DroppedNeedle ships no indexers and no tracker lists. It searches MusicBrainz, an open metadata database that hosts no audio. Every acquisition source it can reach is one you configured yourself: your own slskd instance, or your own Newznab indexers and your own SABnzbd.

Free Music is the one source that ships with the app. It downloads from the Internet Archive, and it offers only items carrying an explicit Creative Commons or public-domain licence, which is shown to you before anything downloads. It needs no account and no API key, it is on by default, and Settings turns it off.

The engine acquires whatever the operator directs it to acquire. It is built for public-domain and Creative Commons recordings, for releases artists distribute themselves through Bandcamp or the Internet Archive's Live Music Archive, for live-taping collections, and for re-acquiring media you already own. Holding the rights to what you download, and to whatever your download client shares back, is your responsibility as the operator.

### Architecture

The backend follows this layered path:

```
Routes (api/v1/routes/)            thin HTTP wrappers, auth-postured
  -> Services (services/native/)   scanner, orchestrator, matcher, file processor
    -> Repositories                 external HTTP: slskd, MusicBrainz, metadata providers
      -> Infrastructure             local stores, tagger (mutagen), fingerprinter
                                    (fpcalc/AcoustID), SQLite WAL, HTTP, SSE
```

Application stores share one SQLite WAL database and one coordinated write lock. Library
Management keeps its immutable plans, filesystem journals, recovery snapshots, and catalog
state there. Content-addressed artwork and recovery blobs live under the persistent cache
mount. Progress is pushed to the frontend over SSE, which fetches with TanStack Query.

### How identification works

`LibraryScanner` walks the configured library paths a folder at a time and identifies each file through four tiers, stopping at the first confident match:

1. **MBID in tags.** The file already carries a MusicBrainz release-group and recording id. Trusted at confidence 1.0, no network call.
2. **Text match.** Fuzzy match of artist, album, and title against MusicBrainz (`rapidfuzz.token_set_ratio`), accepted at confidence >= 0.85.
3. **AcoustID fingerprint.** `fpcalc` fingerprints the audio and AcoustID resolves a recording to a release group, accepted at score >= 0.70.
4. **Manual review.** Nothing confident matched, so the file is queued for an admin to resolve: accept the top candidate, supply an MBID, or reject.

The scan is resumable from a progress ledger, cooperatively cancellable, and incremental (an unchanged mtime and size is skipped). Accepted exact releases also project MusicBrainz release and track artist credits into the catalog. Duplicate artist records converge only when provider evidence proves the same artist MBID; a matching name alone never merges artists.

### Download pipeline

`DownloadOrchestrator` owns the lifecycle: search, score, auto-pick, enqueue, poll, process, import, notify.

1. A request creates a `download_tasks` row and dispatches the orchestrator.
2. It searches via the download client (slskd) and ranks the results with `AlbumPreflightScorer`. Per-track requests use `TrackMatcher`.
3. The top candidate is auto-picked (score >= 0.70), parked for review (0.50 to 0.70), or the task fails (no candidate >= 0.50).
4. A `DownloadManifest` (source peer plus expected filenames and durations) is written to `staging/{task_id}/manifest.json`, then the files are enqueued in slskd.
5. The orchestrator polls slskd for progress until the transfer completes.
6. `FileProcessor` imports each file on its own, continuing on failure so one bad file never aborts the rest of the album.
7. Status resolves to `completed` (all imported), `partial` (some failed), or `failed`.

### Preflight scoring

Candidates are grouped by `(peer, folder)`. Each group gets a coherence score, then a final score:

```
coherence = 0.40 * (file_count / expected_track_count)   # completeness
          + 0.20 * dir_name_similarity                    # folder vs "artist album year"
          + 0.15 * format_consistency                     # all FLAC = 1.0
          + 0.15 * bitrate_consistency                    # low stddev
          + 0.10 * no_junk_bonus                          # not "Various/Unknown"

final = 0.50 * coherence
      + 0.30 * avg_file_confidence
      + 0.10 * upload_speed_signal
      + 0.10 * free_slot_bonus
```

Per-file confidence weights title (0.55), artist-from-path (0.20), and duration tolerance (0.25). An off-version match (remix, live, or acoustic against the original on exactly one side) is penalised by x0.3. A quality gate drops candidates outside the configured codec and quality tier range, and ranking prefers the highest tier absolutely: any acceptable FLAC beats any MP3.

### Verification, import, and quarantine

`FileProcessor` resolves finished files in the download client's directory and verifies them before publication. Tags must be readable, duration must be within the manifest tolerance, and an enabled AcoustID check must agree with the expected recording. A wrong duration or fingerprint is a verification failure.

The accepted files for an acquisition unit are prepared together. The shared publisher writes and validates temporary copies, records filesystem intent, obtains the library-root write lease, publishes every destination, and then adopts the files in one SQLite catalog transaction. Source cleanup starts after that commit. A same-filesystem replacement is atomic at the filesystem level; separate mounts require temporary space for a copy. The journal and compensation logic cover failures between filesystem and SQLite work, which cannot form one atomic transaction.

A `verify_failed`, `corrupt`, `fingerprint_mismatch`, or `duration_mismatch` result records a `download_quarantine` row keyed by `(client_id, peer, filename, release_group)`. The scorer excludes that source from later ranking. Local publication failures and unavailable mounts do not quarantine the peer.

Environment faults, such as a missing or unavailable downloads mount, are not quarantined; they are not the source's fault. The file fails with a sanitised "downloads directory not accessible" reason instead.

### Naming template

Imported files are placed using a template. The variables:

```
{initial} {artist} {album} {albumartist} {year} {track:02d} {title} {ext}
{disc} {genre} {medium} {musicbrainz_id} {artist_mbid}
```

`{initial}` uses the effective album artist (the album artist, falling back to the track artist where needed),
trims Unicode whitespace, ignores a leading case-insensitive `The` plus following Unicode whitespace, and
returns exactly one uppercase alphabetic code point. Empty or non-alphabetic leads return `#`.

The default:

```
{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}
```

This template is the fallback for downloaded imports that are not using an active Library Management profile. Library Scanning never renames a file. An administrator can separately preview organization for existing or scan-discovered files through Library Management.

### Pluggable download client

The engine speaks a `DownloadClientProtocol`, never slskd directly: `client_name`, `is_configured`, `health_check`, `search_album`, `search_track`, `enqueue`, `get_status`, `cancel`, `get_file_path`. Everything client-specific (slskd's `X-API-Key`, search-GUID polling, plain-array enqueue, `(peer, filename)` transfer correlation, Soulseek state-string parsing) lives inside the slskd repository; SABnzbd's API key, NZB enqueue, and history polling live in its own repository. Library layout, MusicBrainz identification, staged publication, tag writing, persistence, ownership checks, quarantine, retry, and scoring live outside both. A protocol conformance test exercises the contract against the slskd mock and a second mock client.

### Cover art

With Lidarr removed, display covers resolve on demand through `AlbumCoverFetcher`: AudioDB, then local sources (an existing library or Jellyfin), then the MusicBrainz Cover Art Archive, then a best-release fallback. First success wins. Library Management uses a separate, pinned artwork decision for the exact MusicBrainz edition, so a later Apply or Undo does not silently switch to whichever display fallback is available. Wikidata is part of the artist-image chain, not the album-cover chain.

### Auth posture

- Library catalog reads and download status: any authenticated user.
- User-scoped download tasks and searches: owner or admin, with ownership checked in the service.
- Scan control, download-client config, quarantine, and Library Management: admin only.
- SSE endpoints require auth on subscribe and are ownership-scoped for download streams.
- API keys are masked on settings reads and never appear in logs. A security test suite enforces the auth matrix, the no-secrets-in-logs guarantee, and key masking.

### Performance

Library reads aggregate from `library_files` (`GROUP BY release_group_mbid`) and are sub-second for a 10k-album library on SQLite WAL. Scan time depends on the active source and workload. DroppedNeedle paces Official at 1 request/second. Built-in BrainzMash uses DroppedNeedle's local 10 requests/second, capacity-1 policy. That is local pacing, not a provider quota or SLA. Subsequent scans are incremental.

---

## Setup

This walks you from a running container to a library that imports downloads. The [Quick start](#quick-start) above covers the compose file and first boot; this covers what to configure in the app afterwards.

`fpcalc` and AcoustID fingerprinting are bundled in the image via `libchromaprint-tools`. An AcoustID API key is optional and enables Tier-3 identification.

### 1. Configure library paths

As admin, go to **Settings > Library** and add your library path(s), using the in-container path (for example `/data/music`). DroppedNeedle validates the path at startup and on save; a non-writable or missing path is reported there rather than crashing the app.

### 2. Configure the download client

Go to **Settings > Download Client** (admin):

1. Enter your slskd URL (for example `http://slskd:5030`).
2. Enter your slskd API key.
3. Click **Test**, then **Save**.

The page shows whether the downloads path is writable and shares a rename boundary with the library. An unavailable or read-only path must be fixed before requesting downloads. A separate-mount warning means imports will work through the slower copy-and-remove fallback. See [slskd setup](#slskd-setup).

### 3. Run a library scan

On **Settings > Library**, click **Scan** (or `POST /api/v1/library/scan/start`). The scan walks your paths, reads tags, identifies files through the tiered strategy, and populates the catalog. It does not edit tags or move music files. Progress streams live. Files it cannot confidently identify land in manual review.

### 4. Optional: configure Library Management

Library Management is the administrator-only write system for tags, artwork, filenames, and folders. It is off by default and separate from Library Scanning.

Open **Library Management** from the administrator sidebar. The action desk puts the normal scan, identity preparation, management preview, and system condition in one view. Each card links to its detailed controls, which start expanded and can be collapsed when you do not need them. Existing bookmarked section links still open the relevant controls. The same workspace is linked from **Settings > Library**.

Profiles control managed tag fields, genres, artwork, naming, sidecars, format compatibility, lyrics, ReplayGain, retention, and media-server refresh. Choose a library default, let a root inherit it or assign an override, then enable automatic triggers independently for acquisitions, Drop/Free imports, and files found by a scan. Creating or assigning a profile does not enable automatic writes.

The built-in **Picard-style Organizer** profile is the recommended starting point. It manages canonical MusicBrainz tags, normalized genres, front artwork, same-root naming and moves, and recognized sidecars while preserving custom tags, timestamps, and permissions. Lyrics, ReplayGain, tag scrubbing, and scan-discovered automation start disabled. Organization stays within the source root unless an administrator chooses a manual cross-root destination. Copy the profile before adding those policies if you want to keep the preset available for comparison. **Existing naming template** is a path-only compatibility profile for installations that only want their previous naming rule.

Lyrics use LRCLIB and are accepted only when title, artist, album, and duration agree; DroppedNeedle does not guess from ambiguous search results. Plain lyrics have verified mappings for every admitted container when that container's writable tag mode is selected. Synchronized lyrics are supported in MP3, FLAC, Ogg, Opus, WMA, and ID3-tagged WAV. When both outputs are selected, M4A and raw AAC safely use plain lyrics as the fallback. A synchronized-only profile blocks formats that cannot represent them.

ReplayGain measures perceived loudness and stores recommended track/album playback gain and peak values in tags. It does not re-encode, normalize, or change the audio samples. A profile can preserve existing values, fill only missing values, or replace them with a fresh album-aware analysis; make it required only if an unavailable or invalid analysis should hold the entire import.

The first activation, and any later change that broadens destructive access, requires a current whole-root dry run and the typed confirmation `CONFIRM`. Enabling another automatic trigger under the same authorized root and profile does not repeat that dry run because it changes when the policy runs, not what it may write. Keep a separate backup of the library before enabling it. DroppedNeedle keeps journals, undo snapshots, first-management baselines, and replaced files in the recycle area, but none of those is a substitute for a backup on another device.

For one-off work, open **Library Management > Manage files**. Select tracks, albums, or roots and create a preview. The preview lists tag, artwork, move, collision, and disk-space effects before Apply becomes available. MusicBrainz release and per-file track mappings must be accepted before DroppedNeedle treats a change as authoritative. Unsupported formats, missing mappings, stale previews, provider failures, and collisions are held for attention instead of being forced through. DroppedNeedle does not automatically overwrite an occupied destination or delete a duplicate source.

When automatic identification cannot prove the physical edition, open the album's
identity panel or choose the release from the Identity readiness report. The built-in
MusicBrainz edition finder starts with the local artist and album title, then shows exact
release dates, countries, formats, labels, catalogue numbers, barcodes, and track counts.
You can also paste a release UUID or canonical MusicBrainz release URL. Search results are
suggestions. **Check this edition** starts the existing evidence review for that exact
release; it never attaches the result directly, substitutes another edition, or bypasses
administrator confirmation. Every indexed file still needs one unique MusicBrainz
release-track mapping before the identity can be accepted.

Operation controls have distinct meanings:

- **Stop** prevents work that has not started. It does not reverse committed items.
- **Undo** restores the state immediately before one completed operation, while that snapshot is still current and retained.
- **Restore baseline** returns managed files to their first-management state. Baselines remain until an administrator previews and confirms a purge.

The control room shows recovery state and any post-commit media-server refresh delivery. A failed external refresh does not roll back files that were already committed.

### 5. Request and watch

Browse or search the MusicBrainz catalogue, open an album, and click **Request**. You can also request a single track from an album's track list. Admin and trusted users' requests start immediately; standard users' requests wait for admin approval. On the **Downloads** page the task moves through `searching -> downloading -> processing -> completed` live over SSE, and on completion the files appear under **Library**.

---

## slskd setup

> **Legality.** DroppedNeedle only orchestrates a user-provided slskd instance over its local HTTP API; it never joins or distributes on the Soulseek/P2P network. You supply, run, and are responsible for slskd and its shared folders.
>
> **Sharing on Soulseek.** Soulseek is a reciprocal network and slskd will not run as a leech-only client: without at least one shared directory (`slskd.yml` -> `shares.directories`), searches and downloads fail. That is a property of Soulseek and slskd, not a DroppedNeedle requirement. Anything you place in a shared directory is distributed to other users of the network. What you share, and whether you hold the right to distribute it, is your decision and your responsibility.

slskd is one of two download sources DroppedNeedle supports. If you are using Usenet instead, skip this section and go to [Usenet setup](#usenet-setup).

DroppedNeedle does not download from Soulseek itself. It talks to your own running slskd instance over slskd's local HTTP API (`X-API-Key`), asks it to search and download, then imports the finished files into your library. You bring slskd; DroppedNeedle drives it.

### Requirements

- slskd 0.25.0 or newer (0.25.1 is the version DroppedNeedle is verified against). Pin it: `slskd/slskd:0.25.1`.
- A Soulseek account configured inside slskd.
- At least one shared folder configured in slskd (see the warning above).
- slskd's HTTP API reachable from the DroppedNeedle container, with an API key.

### The downloads bind-mount

This is the single most common misconfiguration, so read it carefully.

When slskd finishes a download it writes the file into its own downloads directory (`slskd.yml` -> `directories.downloads`), preserving the remote folder structure. DroppedNeedle first tries to move the file into the library atomically. If the container paths cross a mount boundary, it copies the file and removes the source only after the copy succeeds.

For reliable imports, DroppedNeedle must be able to see slskd's downloads directory:

- Expose slskd's completed-downloads directory read-write. A common-parent bind such as `/data` is recommended because the library and downloads remain on one container mount boundary.
- Point `SLSKD_DOWNLOADS_PATH` at the **exact completed-downloads directory** inside that mount, often `/data/slskd/complete`. Pointing it at the parent makes DroppedNeedle search the wrong tree.
- Do not add separate nested binds for `/data/music` or `/data/slskd/complete`. Each bind creates another container mount boundary even when both host paths are on one disk.

DroppedNeedle validates that the path is set, present, and writable. It reports whether fast moves are available and explains the safe copy fallback when the paths use different mount IDs. It still boots to the UI so you can correct a broken path.

```yaml
# In the droppedneedle service of your compose file:
environment:
  - SLSKD_DOWNLOADS_PATH=/data/slskd/complete
volumes:
  - /path/to/media:/data:rw  # library at /data/music; slskd completes at /data/slskd/complete
```

### API key

Configure an API key in slskd (`slskd.yml` -> `web.authentication.api_keys`) and give it to DroppedNeedle under **Settings > Download Client** (URL plus API key). DroppedNeedle sends it as the `X-API-Key` header on every request; it is stored encrypted and is never written to logs.

### Example `slskd.yml` essentials

```yaml
soulseek:
  username: your-soulseek-username
  password: your-soulseek-password

shares:
  directories:
    - /data/share   # REQUIRED: at least one folder of files you share, or you get banned

directories:
  downloads: /data/downloads   # the host path you also bind-mount into DroppedNeedle

web:
  authentication:
    api_keys:
      droppedneedle:
        key: choose-a-long-random-key   # give this to DroppedNeedle's Download Client settings
```

---

## Usenet setup

DroppedNeedle's second download source is Usenet through SABnzbd with Newznab-compatible indexers. The engine searches indexers for albums and tracks, enqueues NZBs in SABnzbd, and imports the finished files using the same verification and import pipeline it uses for slskd.

### Requirements

- [SABnzbd](https://sabnzbd.org/) with an API key.
- One or more Newznab-compatible indexers (NZBGeek, NZBPlanet, NZB.su, Slug, and others) with API keys.
- Expose SABnzbd's completed downloads directory read-write. Keep it under the same common-parent container mount as the library for fast moves; a separate mount uses the safe copy-and-remove fallback.

### Configuration

Go to **Settings > Download Client** (admin), enable Usenet, and enter your SABnzbd URL and API key. Add your Newznab indexers (each one's URL plus your API key) under **Settings > Indexers**; the engine searches all of them and merges the results. Click **Test** on each connection, then **Save**.

slskd and Usenet can be enabled side by side - the source priority control decides which is tried first. Every download goes through the same scoring, verification, quarantine, and import pipeline regardless of where it came from.

---

## Troubleshooting

- **Downloads complete in slskd but nothing imports.** Confirm the completed-downloads path is visible read-write and that `SLSKD_DOWNLOADS_PATH` points at the exact directory. The Download Client settings page shows the path status and reason.
- **Download client shows a separate-mount warning.** Imports remain safe but use copy-and-remove instead of a fast move, temporarily requiring room for both copies. Use one common-parent `/data` bind and remove nested binds to enable fast moves.
- **slskd connection fails or returns 401.** The URL or API key is wrong, or the key is not configured in `slskd.yml`. Re-check both under **Settings > Download Client** and use **Test**.
- **Searches return nothing or you get disconnected.** Confirm slskd has shared folders and a healthy Soulseek connection. Leechers are banned.
- **Scan finds nothing or files go to manual review.** Confirm the library path is correct and readable. Files with no tags and no fingerprint match need manual identification.
- **Tier-3 fingerprinting disabled.** Set an AcoustID API key (optional). Without it, scans rely on Tier 1 and 2 only.
- **MusicBrainz lookups are slow.** DroppedNeedle paces Official at 1 request/second. Built-in BrainzMash uses a local 10 requests/second, capacity-1 policy. That is local pacing, not a provider quota or SLA. Scan duration depends on the source and workload; subsequent scans are incremental.

---

## Recommended stack

DroppedNeedle brings its own library and download engine; you supply the download client. For playback, connect Jellyfin, Navidrome, Plex, or mount your music folder directly into the container.

| Service | Role |
|-|-|
| [slskd](https://github.com/slskd/slskd) (0.25.0+, operator-supplied) | Soulseek download client the native engine drives over its local HTTP API |
| [SABnzbd](https://sabnzbd.org/) (4.x+, operator-supplied) | Usenet download client with Newznab indexer support |
| [MusicBrainz](https://musicbrainz.org/) | Catalogue, identification, and matching |

---

## Authentication

DroppedNeedle is a multi-user application. Every user has a role that controls what they can do.

### Roles

| Role | Requests | Admin access |
|------|----------|--------------|
| **Admin** | Downloaded immediately | Full: manage users, approve/reject requests, change all settings |
| **Trusted** | Downloaded immediately | None |
| **User** | Held for admin approval before downloading | None |

The first account you create at first-run setup is always an admin. After that, an admin manages the rest from Settings > Users, and anyone who signs in through an SSO provider for the first time gets an account automatically.

### Login methods

You sign in with a username and password. Usernames are case-insensitive, and you can change yours later from your profile. Email is optional everywhere; add one if you want it for account linking, but it is never used to sign in.

You can also sign in through Jellyfin, Plex, or any OIDC-compatible provider (Authelia, Keycloak, Authentik, and so on); see [Setting up OIDC](#setting-up-oidc) below. If one of those is your only login, you can add a password from your profile and then sign in by username as well.

Every login method is switched on or off from the web UI. No environment variables are needed.

### Importing users

Instead of creating accounts by hand, an admin can bring in existing users from Jellyfin or Plex. Open Settings > Users, click Import, choose a service, and select the accounts to add; for Plex this includes your Home and managed users as well as your shared friends. No passwords are set during import. Each person signs in with their own Jellyfin or Plex login, and DroppedNeedle links that login to the account the import created for them. Imported users start with the User role, and re-running an import skips anyone already added.

### Sessions

A session lasts 30 days from login and is not extended by activity. Signing out ends your current session. When an admin deletes a user, that user's sessions go with the account.

---

## Features

### Search and request

Search the full MusicBrainz catalogue for any artist or album. Release Types under Settings >
Preferences filter the release groups shown on artist pages and returned by search. Request a whole
album or an individual track, and the native engine handles the download: it searches your download
client, preflight-scores the candidates, picks the best, verifies the files, and imports them into your
library. Admin and trusted users' requests start immediately; requests from standard users are held
in an approval queue until an admin approves or rejects them. A persistent queue tracks all requests,
and you can browse pending and fulfilled requests on a dedicated page with retry and cancel support.

Downloads the engine cannot confidently auto-accept land in a held-import review queue. An admin can preview the audio, accept or reject the import, and supply a MusicBrainz ID before the file is moved into the library.

When a download completes in your download client but DroppedNeedle cannot locate the file, you can trigger a manual reimport from the downloads page to finish the job.

### Wanted

Failed and incomplete download requests are re-searched automatically by a background watcher on an age-based cadence. When it finds a verified match, the download is imported silently. The Wanted tab shows everything the watcher is tracking. You can stop or resume individual watches and mark new finds as seen.

### Quality and storage

Set a quality floor per format (FLAC, MP3, AAC, and others) under Settings > Download Client. Downloads below the cutoff are rejected. When a better copy of an album you already own turns up, the engine replaces the old files and moves the originals to a recycle bin.

A global storage cap and per-user quotas keep the library from filling the disk. Album edition pinning locks a request to a particular release: the engine fills missing tracks from that edition and upgrades individual files as better copies surface. An optional background scan re-checks your library against available sources and imports improvements.

### Built-in player

DroppedNeedle has a full audio player that supports multiple playback sources per track:

- Jellyfin, with configurable codec (AAC, MP3, FLAC, Opus, and others) and bitrate. Playback events are reported back to Jellyfin automatically.
- Navidrome, streaming via the Subsonic API.
- Plex Media Server, with direct-play audio streaming and native Plex scrobbling. Supports multi-library setups.
- Local files, served directly from a mounted music directory.
- YouTube, for previewing albums you haven't downloaded yet. Links can be auto-generated or set manually.

The player supports queue management, shuffle, seek, volume control, a 10-band equalizer with presets, and local plain or time-synchronized lyrics when they are embedded in the managed file.

What you are playing is broadcast live over SSE. Other signed-in users see the current track in real time.

### Connect apps

Third-party music apps can play your library straight from DroppedNeedle, which speaks both the OpenSubsonic and Jellyfin APIs. It is the inbound counterpart to the Jellyfin, Navidrome, and Plex sources: those let DroppedNeedle play from another server, while this lets other apps play from DroppedNeedle.

Turn on either protocol in Settings > Connect Apps and create an app-password, a separate revocable secret for one app so your account password stays private. In the app, enter your server URL with `/subsonic` or `/jellyfin` on the end (for example `https://music.example.com/jellyfin`), your username, and the app-password. Each user manages their own.

Tested with Symfonium, Feishin, and Amperfy over Subsonic, and Finamp, Jellify, Manet, and Symfonium's Jellyfin mode over Jellyfin. When a client asks for a codec the file isn't already in, DroppedNeedle transcodes on the fly if transcoding is enabled; otherwise it sends the original file.

### Discovery

The home page shows trending artists, popular albums, recently added items, genre quick-links, weekly exploration playlists from ListenBrainz, and "Because You Listened To" carousels personalized to your history.

The discover page goes further with a recommendation queue drawn from similar artists, library gaps, fresh releases, global charts, and your listening patterns across ListenBrainz and Last.fm. Each album can be expanded to show the full tracklist and artwork before you decide to request or skip it. Every album has floating preview buttons that stream a short clip from Deezer and iTunes without leaving the page.

You can also browse by genre, view trending and popular charts over different time ranges, and see your own top albums.

Until you link Last.fm or ListenBrainz, the rows that need your listening history stay hidden. You see the shared trending and library sections, plus a prompt to connect an account; link one and your own recommendations fill in.

A per-user weekly mix playlist is built from your listening history and refreshed in the background. When auto-request is enabled (requires an admin standing grant), up to five missing albums from the mix are queued for download automatically.

### Upcoming events

Connect Ticketmaster and Skiddle (free API keys) and DroppedNeedle shows concerts near you. Each user picks as many cities as they like from a geocoded search, each with its own radius. A daily sweep pulls upcoming shows for the artists people follow - or, optionally, for every artist in the library - and a sidebar badge counts the gigs you have not seen yet.

### Following artists

Follow an artist to watch for new releases, and optionally auto-download matching new releases when
they are due (standard users need a one-time admin grant per artist). The Release Types selection also
controls auto-downloads for followed artists. Saving a different Release Types selection establishes a
fresh baseline and does not backfill releases already present. Sidebar badges show how many releases and
gigs you have not seen, backed by per-user seen markers that update via SSE; visiting the page clears them. The
Following hub is a glanceable digest: a 30-day release log with in-library ticks, your next gigs, and
your artist roster, each section one click from its full page.

### Library

Browse your native library by artist or album with search, filtering, sorting, and pagination. View recently added albums and library statistics. Resolve unmatched files from the manual-review queue, preview tag and organization changes through Library Management, rescan albums, and remove albums directly from the UI. Explicit removal deletes the selected files, updates the catalog, and refreshes album and artist statistics.

Jellyfin, Navidrome, Plex, and local file sources each get their own library view with play, shuffle, and queue actions.

### Free Music

Request an album and DroppedNeedle looks for it on the Internet Archive. If it is there under a Creative Commons or public-domain licence, it downloads, gets tagged and organised into your library, and the request is resolved, all without you configuring anything. There is no account to make and no API key to paste. The licence each download is taken under is shown on the task, and linked.

Nothing else is offered. An item with no licence, or an all-rights-reserved one, is never a candidate, so this works for the artists who chose to give their music away and not for anyone else. It is on by default, and one toggle in Settings turns it off.

### Import your purchases

Buy music wherever you like - Bandcamp, the Qobuz store, a label's own shop - then hand the zip or the loose files to DroppedNeedle. Drag them onto the card on your home page or the Import tab on the Downloads page, or click either one to browse. Archives are extracted, and every album is identified by the same pipeline the library scanner uses: MusicBrainz tags first, then AcoustID fingerprints. The files are tagged, organised into your library under your naming template, and if anyone had requested that album, their request is resolved and they get a notification.

Anything DroppedNeedle cannot identify waits under "Needs a match", where you search for the right album and assign it, or discard it.

Drop a better-quality copy of an album you already have and it upgrades in place: the old files go to the recycle bin. An equal or worse copy is skipped. Admin and trusted users can import.

### Where to buy

Album and artist pages show you where to buy the music. Links come from MusicBrainz purchase relationships, with an iTunes fallback (set your region in Settings) and a Bandcamp search behind that, so there is always a way through. Digital, vinyl and CD, and free downloads are listed separately.

Stores are ordered with Bandcamp first.

### Scrobbling

Every track you play can be scrobbled to your own ListenBrainz and Last.fm accounts. Each user links their own accounts from their profile and toggles each service on or off independently. While scrobbling is on, your plays are also saved to a local listening history inside DroppedNeedle, which feeds the Recently Played row on your home page. A "now playing" update goes out when a track starts, and a scrobble is submitted when it finishes.

### Playlists

Create playlists from any mix of Jellyfin, Navidrome, Plex, local, YouTube, and imported Spotify tracks. Reorder by dragging, set custom cover art, and play everything through the same player.

Import playlists from Spotify. Track metadata and album art are pulled on import, and the playlist stays live with periodic SSE refreshes so new tracks you add on Spotify appear automatically.

Playlists are private to you by default. Toggle one to public and it appears read-only for every other signed-in user under "Shared with you", with your name attached; switch it back to private whenever you like. Admins can see that a private playlist exists, along with its track count and owner, but not its name or its tracks.

### Profile

Set a display name and avatar, change your username/email/password, link your own Last.fm and ListenBrainz accounts (with per-user scrobble toggles and a default discovery source), view connected services, and check your library statistics - all from your profile page.

---

## Plugins

Experimental: the plugin API may change until it stabilises.

Third parties can extend DroppedNeedle with scrobblers and purchase-link providers. Install one by pasting a public GitHub repository URL in Settings, or by copying a folder into the plugins directory. No plugin capability downloads music, and DroppedNeedle never calls plugin code to acquire anything.

A plugin is Python running in-process with your server's full privileges, and there is no sandbox. Installing downloads the code and nothing more; the plugin does nothing until an admin enables it. Read the code before you do. DroppedNeedle bundles no plugins and endorses none - a worked example ships in `examples/plugins`.

The full API reference is in [PLUGINS.md](PLUGINS.md).

---

## Integrations

| Service | What it does |
|-|-|
| [slskd](https://github.com/slskd/slskd) (operator-supplied) | Soulseek download client the native engine drives over its local HTTP API |
| [SABnzbd](https://sabnzbd.org/) (operator-supplied) | Usenet download client with Newznab indexer support |
| [MusicBrainz](https://musicbrainz.org/) | Exact release identities, canonical tags and credits, artist reconciliation, and catalog search |
| [AcoustID](https://acoustid.org/) | Audio fingerprinting for Tier-3 scan identification (optional API key) |
| [LRCLIB](https://lrclib.net/) | Exact-match plain and synchronized lyrics for opt-in Library Management profiles |
| [Cover Art Archive](https://coverartarchive.org/) | Album artwork |
| [TheAudioDB](https://www.theaudiodb.com/) | Artist and album images (fanart, banners, logos, CD art) |
| [Wikidata](https://www.wikidata.org/) | Artist descriptions and external links |
| [Jellyfin](https://jellyfin.org/) | Audio streaming and library browsing |
| [Navidrome](https://www.navidrome.org/) | Audio streaming via Subsonic API |
| [Plex](https://www.plex.tv/) | Audio streaming and library browsing via Plex Media Server |
| [ListenBrainz](https://listenbrainz.org/) | Listening history, discovery, scrobbling, weekly playlists |
| [Last.fm](https://www.last.fm/) | Scrobbling and listen tracking |
| YouTube | Album playback when no local copy exists |
| Local files | Direct playback from a mounted music directory |
| [Spotify](https://www.spotify.com/) | Playlist import with live sync |
| [Ticketmaster](https://www.ticketmaster.com/) | Upcoming concert discovery |
| [Skiddle](https://www.skiddle.com/) | Upcoming concert discovery |
| [Deezer](https://www.deezer.com/) | Short audio previews on the discover page |
| iTunes | Short audio previews on the discover page |

All integrations are configured through the web UI. No config files or environment variables needed beyond the basics listed below.

MusicBrainz source choices are under **Settings > MusicBrainz**. **BrainzMash** is built in, the default for fresh and reset configuration, and the **Recommended** choice. **Official**, a **Self-hosted mirror**, and a **Community / external server** can each be saved as the persistent source. DroppedNeedle never silently falls back between sources. The BrainzMash disclosure says it receives MusicBrainz query terms and normal connection metadata.

See the [MusicBrainz sources and self-hosted mirrors guide](docs/musicbrainz-mirror-selfhosting.md) for source behavior, upgrade migration, consent, and mirror setup.

---

## Configuration

DroppedNeedle stores its config in `config/config.json` inside the mapped config volume. Everything is managed through the UI.

### Environment variables

| Variable | Default | Description |
|-|-|-|
| `PUID` | `1000` | User ID for file ownership inside the container |
| `PGID` | `1000` | Group ID for file ownership inside the container |
| `UMASK` | `027` | Octal file-creation mask. `027` allows owner writes and group reads; `002` allows trusted group members to write. Invalid values stop startup. |
| `PORT` | `8688` | Port the application listens on |
| `TZ` | `Etc/UTC` | Container timezone |
| `SLSKD_DOWNLOADS_PATH` | `/data/downloads/slskd` | Exact in-container path to slskd's completed downloads. The Compose example overrides this with `/data/slskd/complete`; keep either path inside the library's common-parent mount for fast moves. |

Run `id` on your host to find your PUID and PGID values.

> **Unraid, TrueNAS, and other NAS users:** Unraid commonly uses `nobody:users`
> (PUID=99, PGID=100). Set PUID and PGID to the account that owns the mounted config and
> cache paths. The container works without `chown` when those paths are already writable,
> which covers FUSE/shfs, NFS, CIFS, and non-root containers that reject ownership
> changes. A read-only config or cache mount is refused before an upgrade changes data.

`UMASK` controls permissions on newly created files; it does not rewrite existing
permissions. Keep the default `027` for a private deployment. Use `002` when DroppedNeedle
and another trusted service share a group and both must modify the same media. Avoid `000`:
it makes new files writable by every local account allowed by the underlying filesystem.
A move or metadata-preserving copy can retain a source file's existing mode, so `UMASK`
is not a way to override permissions supplied by a download client.

### In-app settings

| Setting | Location |
|-|-|
| Library paths, naming template, scan schedule, AcoustID key | Settings > Library |
| Library Management profiles, root assignments, automatic triggers, previews, recovery, and history | Library Management |
| slskd URL and API key, SABnzbd/Usenet URL and API key, Newznab indexers, quality tiers, verification, wanted watcher | Settings > Download Client |
| OpenSubsonic and Jellyfin APIs that let apps stream your library, app-passwords, transcoding | Settings > Connect Apps |
| Jellyfin URL and API key | Settings > Jellyfin |
| Navidrome URL and credentials | Settings > Navidrome |
| Plex URL, token (OAuth or manual), music libraries, scrobble toggle | Settings > Plex |
| Local files directory path | Settings > Local Files |
| Last.fm app key + shared secret (admin; one app for the whole instance) | Settings > Last.fm |
| YouTube API key | Settings > YouTube |
| Spotify client ID and secret, playlist import | Settings > Spotify |
| Ticketmaster and Skiddle API keys, sweep scope and daily check time | Settings > Live Events |
| Link your own Last.fm + ListenBrainz, per-user scrobble toggles, default discovery source | Profile > Scrobbling & Discovery |
| Home page layout and Release Types for artist pages, search, and followed-artist auto-downloads | Settings > Preferences |
| MusicBrainz source and connection settings | Settings > MusicBrainz |
| AudioDB settings and cache TTLs | Settings > Advanced |
| HSTS header and HIBP password breach checking | Settings > Security |
| User accounts, roles, and user import (Jellyfin/Plex) | Settings > Users |

### Setting up Last.fm

1. **Admin, once per instance:** register an app at [last.fm/api/account/create](https://www.last.fm/api/account/create) to get an API key and shared secret, and enter them in Settings > Last.fm.
2. **Each user:** open Profile > Scrobbling & Discovery, click Connect on Last.fm, authorise in the popup, then choose Finish. Your account is linked and the scrobble toggle is yours.

### Setting up ListenBrainz

1. Copy your user token from [listenbrainz.org/profile](https://listenbrainz.org/profile/).
2. In Profile > Scrobbling & Discovery, click Connect on ListenBrainz and paste your username + token.

### Setting up OIDC

Any OIDC provider that supports the authorization code flow works (Authelia, Keycloak, Authentik, etc.).

1. In your provider, create a new OIDC client / application. Set the redirect URI to `https://your-droppedneedle-url/api/v1/auth/oidc/callback`.
2. In Settings > Security, enter your provider's **Issuer URL**, **Client ID**, and **Client Secret**.
3. Save, an SSO button will appear on the login page.

Users who sign in via OIDC are created automatically on first login, given an auto-generated username, and assigned the **User** role by default. An admin can promote them from Settings > Users.

### Security settings

Settings > Security exposes two features (admin only):

**Password breach checking (HIBP):** When enabled, new passwords are checked against the [Have I Been Pwned](https://haveibeenpwned.com/Passwords) breach database using the k-anonymity API (`api.pwnedpasswords.com`). Only the first 5 characters of the password's SHA-1 hash are transmitted, the full password never leaves the server. This is on by default. For air-gapped or offline installs, you can either disable it or supply the path to a local copy of the HIBP hash file (download the "ordered by hash" version from haveibeenpwned.com/Passwords, typically ~35 GB). When a local path is configured, no outbound network calls are made.

**HSTS (Strict-Transport-Security):** Only relevant if you're serving DroppedNeedle over HTTPS via a reverse proxy. Leave this disabled for plain HTTP installs. Enabling it on HTTP will cause browsers to refuse to connect until the HSTS entry expires. When behind HTTPS, set a `max-age` to instruct browsers to always use HTTPS. Starting with a shorter value (e.g. 30 days) and increasing it once you're confident everything works is recommended.

### TheAudioDB

AudioDB provides richer artist and album artwork from a fast CDN. It's enabled by default with the free public API key, which is rate-limited to 30 requests per minute. Premium keys from [theaudiodb.com](https://www.theaudiodb.com/) unlock higher limits.

Under Settings > Advanced, you can toggle AudioDB on or off, switch between direct CDN loading and proxied loading (for privacy), enable name-based search fallback for niche artists, and adjust cache TTLs.

---

## Playback sources

### Jellyfin

Audio is transcoded on the Jellyfin server and streamed to the browser. Supported codecs include AAC, MP3, Opus, FLAC, Vorbis, ALAC, WAV, and WMA. Bitrate is configurable between 32 kbps and 320 kbps. Playback start, progress, and stop events are reported back to Jellyfin.

### Local files

Mount your music directory into the container and DroppedNeedle serves files directly. The mount path inside the container must match the Music Directory Path set in Settings > Local Files.

```yaml
volumes:
  - /path/to/your/music:/music:ro
```

### Navidrome

Connect your Navidrome instance under Settings > Navidrome.

### Plex

Connect Plex under Settings > Plex. You can sign in with Plex OAuth or paste in a token yourself. Once you're connected, choose the music libraries you want to include. If you pick more than one, DroppedNeedle merges them into a single library view.

Tracks play directly from Plex with no server-side transcoding. The DroppedNeedle backend proxies the stream so your Plex token never reaches the browser.

Plex scrobbling is on by default. Turn it off in Settings > Plex or from the library page if you'd rather rely on Last.fm and ListenBrainz instead.

### YouTube

Albums can be linked to a YouTube URL and played inline. This is useful for listening to albums before you've downloaded them. Links can be auto-generated with a YouTube API key or added manually.

A note on reliability: YouTube playback depends on the embedded player, which can be finicky. It works best in a browser where you're signed into YouTube, and VPNs tend to cause issues. Treat it as a convenience for previewing albums rather than a primary playback source.

---

## Volumes and persistence

| Container path | Purpose |
|-|-|
| `/app/config` | Application config (`config.json`) |
| `/app/cache` | Cover art cache, metadata cache, SQLite databases |
| `/app/plugins` | Installed plugins. Mount it, or plugins you install disappear when the container is recreated |
| `/app/imports` | Persistent staging for the drop importer (optional) |
| `/data/music` | Recommended music library root inside the common media mount |
| `/data/slskd/complete` | Recommended slskd completed-downloads path inside the common media mount |

Map `/app/config`, `/app/cache`, and `/app/plugins` to persistent host directories so they survive container restarts. For fast imports, expose the library and completed downloads through one common-parent mount such as `/data`; separate or nested binds use the safe copy-and-remove fallback and temporarily require room for both copies. Linux paths are case-sensitive, so host-path casing must match exactly. See [slskd setup](#slskd-setup). `/app/imports` is optional, but leave it unmounted and large uploads land on the container's writable layer, while anything waiting for a manual match is lost when the container is recreated.

---

## API

Interactive API docs (Swagger UI) are available at `/api/v1/docs` on your DroppedNeedle instance.

All `/api/v1/*` routes require authentication (a Bearer token or the `droppedneedle_session` cookie), aside from a small public allowlist for setup, login, and provider discovery. Everything under `/api/v1/settings/*` additionally requires the **Admin** role.

A health check endpoint is at `/health`.

---

## Development

See the [CONTRIBUTING](CONTRIBUTING.md) guide for instructions on setting up a development environment, running tests, and submitting contributions.

---

## Support

Documentation is at [droppedneedle.com](https://www.droppedneedle.com/).

For questions, help, or just to chat, join the [Discord](https://discord.gg/B5suDg7gu2). Bug reports and feature requests go on [GitHub Issues](https://github.com/DroppedNeedle/DroppedNeedle/issues).

If you find DroppedNeedle useful, consider supporting development:

[![ko-fi](https://ko-fi.com/img/githubbutton_sm.svg)](https://ko-fi.com/M4M41URGJO)
<a href="https://github.com/sponsors/HabiRabbu"><img src="https://img.shields.io/badge/Sponsor%20this%20project-ea4aaa?style=for-the-badge&logo=github&logoColor=white" alt="Sponsor this project" style="border-radius: 6px; height: 30px" /></a>

Monthly sponsorships keep development sustainable and unlock a few perks for you:

| Tier | Amount | Perks |
| --- | --- | --- |
| Supporter | $5/month | Sponsor badge on your GitHub profile |
| Backer | $20/month | All of the above, plus your name in the README credits and a private Discord role |
| Patron | $50/month | All of the above, plus early access to beta builds and a vote on upcoming features |
| Guardian | $100/month | All of the above, plus your name or logo on the website and prioritized bug reports and feature requests |

Prefer to give once? One-time donations are welcome through [GitHub Sponsors](https://github.com/sponsors/HabiRabbu) or [Ko-fi](https://ko-fi.com/M4M41URGJO).

---

## Sponsors

Thanks to everyone who supports the project - your sponsorship keeps development sustainable.

<!-- Add sponsors here as they join, e.g.:
- [@username](https://github.com/username) - Patron
-->

Become a sponsor on [GitHub Sponsors](https://github.com/sponsors/HabiRabbu) or [Ko-fi](https://ko-fi.com/M4M41URGJO).

---

## License

DroppedNeedle is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0). Copyright (c) 2025 Harvey Bragg and contributors.

For commercial licensing enquiries, contact the maintainer.
