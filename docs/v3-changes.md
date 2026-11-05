# What changed in v3

Things v2 did that v3 does differently or no longer does. Read this before
you upgrade. The upgrade itself is in [Upgrading from v2](upgrading-from-v2.md),
and the callback addresses and settings to change first are under
[Before you upgrade](upgrading-from-v2.md#before-you-upgrade).

## Contents

- [Removed](#removed)
- [Changed](#changed)
- [Moved](#moved)

## Removed

- **The v2 API.** Nothing under `/api/v1` answers in v3. A script that
  called it has to move to `/api/v3`, whose shapes differ.
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
- **Library Management options v3 does not act on.** The artwork source
  list, the approved-only artwork switch, size and picture-type rules, cover
  file naming and overwriting, the ID3 version and the other per-format tag
  policies, genre ancestry depth, source cleanup, empty folder removal, the
  file safety switches (keep timestamps, refuse links, check the file after
  writing), the lyrics provider and the DroppedNeedle refresh switch are gone.
  So are the folder overrides for source cleanup, keep timestamps and edition
  acceptance, and the external refresh retry count and delay. A carried
  profile keeps its other options. The import report names each one it drops.
- **v2 profile share codes and files.** A Library Management profile share
  code (`DNLP1:`) or `.dnprofile` file made in v2 does not import into v3.
  Share the profile again from v3 to get a code or file v3 reads.
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
  provider answers per source and cached images, shows the database size,
  and clears the provider data or the images.
  Provider answers are kept in `metadata-cache.db` and survive a restart;
  saving a setting never clears them.
- **Settings nothing used, and the cache and pacing knobs.** These v2
  settings are gone. Most were never read; the cache lifetimes and the
  library image pacing now use fixed values equal to v2's defaults. An
  import of a v2 export carrying them goes ahead, and its report lists
  each one as `dropped_setting`. The server-wide home page section settings
  are not exported at all, so they show under `left_behind` instead.
  - The server-wide primary music source. Each person's own choice stays
    in their profile; a discovery section with no source named asks
    ListenBrainz first.
  - Settings > Advanced: every server and browser cache lifetime
    (including TheAudioDB's), the memory and disk cache limits, the HTTP
    timeouts and pool size, the library image batch sizes, delays and
    workers, the image refresh's stall and run timeouts, the Discover
    queue's lifetime, card details cache and status polling interval, the
    genre section lifetime, request concurrency and the orphan cover
    cadence.
  - Settings > Download Client: slskd's verification setting and SABnzbd's
    client type and post-processing level. slskd's own copies of the
    quality, scoring, timeout, retry and concurrency settings are not listed:
    they move into the download policy on an install that has none, and v2
    did not read them on one that has.
  - Download policy: "Verify downloads", the Usenet release age before
    blocklisting, the source selection mode and the target bitrate of a
    custom MP3 recipe step.
  - The server-wide scrobble targets (each user's own scrobble switches
    stay), the server-wide home page section settings (each user's own
    section choices stay), "Scrobble to Plex" (a Plex track
    that ends is always reported), Connect Apps' exact-track approval flag,
    MusicBrainz's concurrent searches, and the scan schedule's stored "last
    scan" (it now comes from the scan history).

  The outbound HTTP timeouts are environment variables, `HTTP_TIMEOUT` and
  `HTTP_CONNECT_TIMEOUT`, as before.
- **5 and 10 minute scan schedules.** v3 has neither. A schedule of either
  becomes 30 minutes.
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

## Changed

- **One artist, one entry.** v2 made a second artist when files of one
  artist had different sort names or capitals. v3 keeps them on one record,
  and joins the ones the upgrade carried at the first scan, with an Undo in
  Duplicate artists. Artists MusicBrainz says are different stay apart.
- **Held downloads expire.** v2 kept a held download until you acted on it.
  v3 removes a held file or upload 90 days after it was held. Ones carried
  from v2 get 90 days from the upgrade.
- **Quality settings.** A v2 quality range becomes the matching quality
  recipe, with FLAC at every resolution for "lossless". A policy capped at
  16-bit and 48 kHz or lower, as v2's default was from August 2026, gets FLAC
  up to CD quality instead, so a 24-bit folder is not downloaded automatically,
  as v2 refused it. Automatic downloads take only what the recipe lists, even
  if v2 had "FLAC and MP3 only" turned off: you can still pick another format
  by hand, and one that lands anyway is held for you. The other lossless caps
  and preferences, the lossy bitrate bounds and the unknown-quality setting
  are not applied, and the import report names any you had changed.
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
  Undo and "Restore original state" put the values back; the tag stays
  ID3v2.4.
- **Other people's public playlists in music apps.** In v2 Subsonic and
  Jellyfin apps listed them next to your own. In v3 apps list your own
  playlists only, for every user, until you turn on the switch under
  Profile > Connect Apps. An app that saved a public playlist's id can still
  open it, and the web UI lists what it did before.

## Moved

- **Recycle bin.** The recycle bin folder and how long removed files are
  kept moved from the download settings to Library Management. The upgrade
  carries the folder your removals used; the report names any path it
  dropped.
- **Playlist "request missing" for scripts.** v2 had a server route,
  `POST /api/v1/playlists/<id>/request-missing`. The button on the playlist
  page works as before, but v3 has no such route: the page works out the
  missing albums itself and sends them to `POST /api/v3/requests/batches`.
  A script that called the old route has to do the same.
