# Configuration

Everything user-editable lives in the web UI and is stored in the database, so a database backup holds it. The environment is only for container basics. This page lists the environment variables, the folders to persist, where each setting lives in the UI, and how to update.

## Contents

- [Environment variables](#environment-variables)
- [Network and reverse proxy](#network-and-reverse-proxy)
- [Folders to persist](#folders-to-persist)
- [Storage and permissions](#storage-and-permissions)
- [Where settings live](#where-settings-live)
- [Updating and image tags](#updating-and-image-tags)
- [Running without Docker](#running-without-docker)

## Environment variables

| Variable | Default | What it is |
|-|-|-|
| `PUID` | `1000` | File owner inside the container (run `id` on the host) |
| `PGID` | `1000` | File group inside the container |
| `UMASK` | `027` | Creation mask for new files; `002` for trusted group-writable media |
| `PORT` | `8688` | Port the app listens on |
| `BIND_HOST` | `auto` | `auto` listens on IPv4 and IPv6, dropping to IPv4 alone where IPv6 is off. Set `0.0.0.0`, `::`, or one interface IP to pin it |
| `TRUSTED_PROXY_IPS` | `127.0.0.1,::1` | IPs/CIDRs whose `X-Forwarded-*` headers are trusted; point it at your reverse proxy, listing every address family it arrives on. `*` trusts every peer: only behind a proxy that strips spoofed headers |
| `BASE_PATH` | empty | Serve under a sub-path behind a reverse proxy, e.g. `/music`. Letters, digits and `.` `_` `~` `-` only, no trailing slash, and its first segment cannot be `api` |
| `TZ` | unset | Container time zone, as an IANA name such as `Europe/London`. Daily library scans run at their set time in this zone. Unset, they use the system's zone (`/etc/localtime`, UTC in the image); a name the server does not know runs them in UTC, and the scan settings say so |
| `SLSKD_DOWNLOADS_PATH` | `/data/downloads/slskd` | Exact in-container path to slskd completions (the compose example uses `/data/slskd/complete`) |
| `LOG_LEVEL` | `INFO` | `TRACE`, `DEBUG`, `INFO`, `WARNING` (or `WARN`), `ERROR` or `CRITICAL` (the same as `ERROR`). The audio decoders log only errors whatever this says; set `RUST_LOG` to see more |

<details>
<summary>Rarely needed variables</summary>

| Variable | Default | What it is |
|-|-|-|
| `CONTACT_EMAIL` | `contact@droppedneedle.com` | Contact address in the outbound User-Agent; MusicBrainz asks for one, so set your own if you run many lookups |
| `DISCOVER_WARMER_ENABLED` | `true` | Keeps each user's Discover page and discover queue fresh in the background while they use them. `false` builds them only when someone opens the page |
| `ALLOW_PASSWORD_LOGIN` | `true` | Username and password sign-in. `false` turns off password login, recovery and changes, so people sign in only through Plex, Jellyfin or OIDC; first-admin setup, app passwords and the v2 admin password on the upgrade page still work. Set it back to `true` and restart if the identity provider is unreachable, and before using a host recovery code ([Forgotten passwords and recovery](accounts-and-sign-in.md#forgotten-passwords-and-recovery)) |
| `SHUTDOWN_GRACE_PERIOD` | `10` | Seconds to finish open requests and stop background work on shutdown |
| `HTTP_TIMEOUT` | `30` | Outbound request timeout, seconds |
| `HTTP_CONNECT_TIMEOUT` | `10` | Outbound connect timeout, seconds |
| `HTTP_MAX_KEEPALIVE` | `50` | Idle outbound connections kept per host |
| `RUST_LOG` | unset | Full log filter (e.g. `info,droppedneedle_library=debug`, or `droppedneedle_auth=debug` for sign-in); wins over `LOG_LEVEL` |
| `ROOT_APP_DIR` | `/app` | Base for the cache, database, config and web UI paths below, and for `/app/plugins` and `/app/imports` |
| `CACHE_DIR` | `/app/cache` | Database, backups, served web UI, caches |
| `COVER_CACHE_MAX_SIZE_MB` | `500` | Size cap for downloaded and extracted cover art under `CACHE_DIR/covers`; the least recently shown covers go first |
| `LIBRARY_DB_PATH` | `/app/cache/library.db` | The database file; backups go in `backups/` and Undo's saved copies in `undo/` next to it. `droppedneedle-tool restore` writes a backup back under this file name |
| `CONFIG_DIR` | `/app/config` | Holds `data_enc.key`, the key that seals stored secrets. `CONFIG_FILE_PATH` is no longer read and stops the start: remove it, and if it named a `config.json` outside `ROOT_APP_DIR/config`, move that file there first, beside v2's `.env` |
| `DROPPEDNEEDLE_STATIC_DIR` | `/app/static` | The web UI build shipped in the image |
| `TMPDIR` | `/tmp` | Folder for plugin stream spools, up to 2 GB at once |

</details>

The config folder holds the key that seals stored secrets. A `config/config.json` from an earlier v3 is copied into the database once, at the first start, and left as it was. You can delete it once you are settled on this version; while it is there, a fresh database beside it would copy it in again.

## Network and reverse proxy

The app answers on IPv4 and IPv6, but Docker still has to publish the port on both. `docker port droppedneedle` should list `0.0.0.0:8688` and `[::]:8688`; if only the first appears, turn on IPv6 for the daemon (`"ipv6"` and `"ip6tables"` in `/etc/docker/daemon.json`). Pinning `BIND_HOST` to one interface IP answers only there, but the container `HEALTHCHECK` still uses localhost, so keep a wildcard or loopback value unless you check that address yourself.

Behind a reverse proxy, list the proxy in `TRUSTED_PROXY_IPS` and have it pass `Host` and `X-Forwarded-Proto` through. Serve DroppedNeedle over HTTPS through the proxy if you can. Browsers use HTTP/2 only over HTTPS, and HTTP/2 loads cover grids faster than plain HTTP, where six connections per host can all be busy with slow covers. The proxy headers, `BASE_PATH`, the public address, HSTS and the content-security policy are covered in [Reverse proxy and security](reverse-proxy-and-security.md).

## Folders to persist

| Data | Container path | Notes |
|-|-|-|
| Encryption key (`data_enc.key`) | `/app/config` | Persist it, and keep it with your backups. Settings are in the database, under `/app/cache` |
| Database, backups, cover art and metadata cache | `/app/cache` | Persist it. The `undo` folder beside `library.db` holds the tags, pictures and files Undo and "Restore original state" put back; backups copy it into `backups/undo`, so keep the whole `backups` folder. Provider answers (MusicBrainz, Last.fm and the like) live in `metadata-cache.db`, which survives a restart, is not in the backups, and can be deleted safely while the server is stopped |
| Plugins | `/app/plugins` | Persist it or installs vanish on recreate |
| Drop-import uploads | `/app/imports` | Persist it. Uploads wait here until they are matched; without it they live on the container layer and vanish on recreate |
| Media | `/data` | Shared parent for library (`/data/music`) and client completions |

Backups are covered in [Backups and restore](backups-and-restore.md).

## Storage and permissions

Keep `UMASK=027` for a private box. Use `002` when DroppedNeedle and another trusted service in the same group both write the same media. Skip `000`: it makes new files writable by every local account the filesystem allows. `UMASK` shapes new files only; a move can keep the source mode the download client set.

Unraid commonly uses `nobody:users` (PUID 99, PGID 100). Point PUID and PGID at whoever owns the mounted config and cache paths. After a PUID change, back to 1000 included, the next start hands the files the old id owns in `/app/config`, `/app/cache`, `/app/plugins` and `/app/imports` to the new one. The container skips ownership changes it cannot make, which covers FUSE, NFS, CIFS, and rootless setups. A read-only config or cache mount refuses to start before anything gets written. On every start the container also makes `/app/plugins` and `/app/imports` writable by PUID and PGID, mounted or not, so a folder Docker created owned by root works. One it cannot make writable only logs a warning, and a link or a file at either path is left as it is.

`/app/config` and `/app/cache` must be writable and honor SQLite locking, `fsync`, and atomic replacement. Plain bind mounts, named volumes, Unraid pool or disk paths, and TrueNAS datasets with normal permissions all qualify. The database (`/app/cache/library.db` unless `LIBRARY_DB_PATH` moves it) must be on a local filesystem: DroppedNeedle refuses to start with it on NFS, SMB/CIFS or a FUSE mount, whatever locking the mount offers, or behind a symlink.

On Unraid, keep `/app/cache` on a pool path such as `/mnt/cache/appdata/droppedneedle/cache`, as the template does: a `/mnt/user` share is a FUSE mount unless it is set to exclusive access. An exclusive share works too. With no pool, use an array disk path such as `/mnt/disk1/appdata/droppedneedle/cache`: a pool name that does not exist puts the database in memory, and it is gone at the next reboot. If `LIBRARY_DB_PATH` puts the database on a local disk while `/app/cache` is on a network or FUSE mount, or is a symlink, `metadata-cache.db` is kept beside the database instead; if that place is refused too, provider answers are fetched each time and the server log says the provider cache is off. On Docker Desktop for Windows, prefer named volumes for those two paths.

A database on tmpfs or ramfs (a folder held in memory) still starts, but it and its backups are lost when the machine restarts, and a Docker `tmpfs` mount is emptied each time the container stops. The server log warns at boot and Settings > Backups shows a warning. On Unraid this happens when the Data path's `/mnt/cache` names a pool the box does not have. To fix it there, stop the container, move the folder to a pool or disk path such as `/mnt/disk1/appdata/droppedneedle/cache`, point Data at it and start the container again. On a `tmpfs` mount, stopping the container loses everything, so first press Back up now under Settings > Backups and copy the `backups` folder out while the container runs: `docker exec droppedneedle tar -C /app/cache -cf - backups > backups.tar` (`docker cp` cannot read a tmpfs mount). Then stop it, mount a disk folder at `/app/cache`, unpack the copy into that folder with `tar -xf backups.tar -C /path/to/cache`, and [restore the newest backup](backups-and-restore.md#restoring-a-backup) before you start the server.

For the media mount, one shared parent (`/data`) with the library and the download client's completions under it keeps imports fast; see [Download clients](download-clients.md#slskd).

### Import folder

The [import folder](downloads.md#import-folder) is a folder you mount read-write and name under Settings > Library. It must not be a library folder, the recycle bin, a download client's or download plugin's folder, `/app/imports`, `/app/config` or `/app/cache`, and must not sit inside or hold one of them: DroppedNeedle checks this on every pass and imports nothing while it overlaps. A finished download never reads or removes anything inside the import folder or the recycle bin, even where a download folder holds them. A read-only mount imports nothing either, because removing what went in is part of the job. On the same disk as the library, files go in faster.

```yaml
services:
  droppedneedle:
    volumes:
      - /mnt/media/music:/data/music
      - /mnt/media/to-import:/data/import   # then set /data/import under Settings > Library
```

## Where settings live

| Setting | Location |
|-|-|
| Library paths, import naming script, scan schedule, folder watching, AcoustID key, storage cap | Settings > Library |
| Prefer local cover art before online covers | Settings > Library > Covers |
| Library Management profiles, previews, recovery, the recycle bin folder and retention | Library Management (admin), also reached from Organize files under Settings > Library |
| Download clients (slskd, SABnzbd, NZBGet), Deemix ([below](#deemix)), source priority, quality tiers, picking a copy, wanted watcher, Usenet retention days | Settings > Download Client |
| Newznab indexers or Prowlarr | Settings > Indexers / Prowlarr |
| Edition preferences, managing unconfirmed matches | Settings > Editions |
| Lidarr import ([below](#lidarr-import)) | Settings > Lidarr Import |
| Subsonic and Jellyfin APIs, app passwords, transcoding, apps that sign in through the browser | Settings > Connect Apps |
| Jellyfin | Settings > Jellyfin |
| Navidrome | Settings > Navidrome |
| Plex | Settings > Plex |
| Local files (the built-in library) | Settings > Library |
| Last.fm app key (admin, once per instance; the account that owns it needs Last.fm Pro from 1 December 2026) | Settings > Last.fm |
| YouTube API key | Settings > YouTube |
| Spotify client ID and secret | Settings > Spotify |
| Ticketmaster and Skiddle keys, sweep scope | Settings > Live Events |
| ListenBrainz recommendations, on by default (admin) | Settings > ListenBrainz |
| Buy links | Settings > Get it |
| Scrobbling and discovery accounts | Profile > Scrobbling & Discovery |
| Release types | Settings > Release Types |
| Home layout | Settings > Home |
| Discover page sections, the discover queue | Settings > Discover |
| Sidebar | Settings > Sidebar |
| Your own main listening source (ListenBrainz or Last.fm) | Profile > Scrobbling & Discovery; Settings > Music Source sets the same for the admin using it |
| MusicBrainz server (official or your own mirror) | Settings > MusicBrainz |
| Database backups (admin) | Settings > Backups |
| Users, roles, Jellyfin and Plex user import | Settings > Users |
| Password breach checking, HSTS, the content-security policy, single sign-on (OIDC), who can download library files | Settings > Security |
| Plugins | Settings > Plugins |
| Library images, cache counts and clearing | Settings > Cache |
| Wrapped API key ([Wrapped API](accounts-and-sign-in.md#wrapped-api)) | Settings > Wrapped API |
| Outbound provider calls, rate limits and background jobs | Settings > Diagnostics |
| Discover queue and top picks tuning | Settings > Advanced |
| AudioDB images: on or off, key, name search fallback, direct remote images | Settings > Advanced |
| Request history, ignored albums and clean-up interval | Settings > Advanced |
| App version, update check and release notes | Settings > About |
| Theme and your own CSS (kept on your account) | Profile > Appearance |
| Site CSS for every signed-in page, and whether people may add their own (admin) | Settings > Appearance |

### Deemix

Settings > Download Client (admin) has the Deemix card, which saves three values:

- **Turned on**: off by default. Deezer is a download source only while this is on and a URL is set.
- **URL**: Deemix's address, such as `http://deemix:6595` on the private network in [Deemix](download-clients.md#deemix).
- **Downloads folder**: where DroppedNeedle sees Deemix's download folder. The default is `/deemix-downloads`. Set it to the folder mounted as Deemix's `/downloads`.

There is no quality setting: the quality comes from your quality list and your Deezer account. DroppedNeedle never changes Deemix's own settings.

### Lidarr import

Settings > Lidarr Import (admin) adds the artists your Lidarr monitors to your follows, so their new releases reach you as they would for any [followed artist](downloads.md#following-artists).

1. Enter your Lidarr address and its API key (in Lidarr under Settings > General > Security), Test, then save.
2. Under Artist sync, every artist Lidarr monitors is listed, except any without a valid MusicBrainz id. Those you already follow are marked Following; the rest are ticked. Untick any you don't want and press Import.

When Lidarr monitors every new album of an artist, the list marks it Auto-download, and the new follow gets auto-download turned on. An artist you already follow keeps its own auto-download setting. The follows go to the admin who runs the import. DroppedNeedle only reads Lidarr's status and artist list and changes nothing in Lidarr. It does not sync by itself: open the tab again and import to pick up artists added to Lidarr since.

### Custom CSS

Profile > Appearance > Custom CSS takes your own CSS rules and applies them to the signed-in app once you save (not the sign-in, setup or app connect pages). Your theme and your CSS are kept on your account, so they follow you to every browser and device you sign in on; each browser also keeps a copy so a reload paints at once. Another account on the same browser does not get them, and signing out takes them off until you sign in again. Up to 20,000 characters. If you used custom CSS or chose a theme before they moved to the account, the first sign-in on that browser moves them up by itself.

The editor checks your CSS as you type, can preview it live before you save, and keeps your last 10 saves on your account (clearing the sheet leaves them there). [Custom CSS](custom-css.md) covers the editor, the variables and the style hooks you can rely on.

An admin can add site CSS under Settings > Appearance (up to 50,000 characters). It loads on every signed-in page on top of the app's own styles, and each person's CSS loads on top of it. The same section has "Let people add their own CSS", on by default: while it is off, nobody's own CSS is applied or can be saved, and their text is kept. An admin can also turn one person's CSS off under Settings > Users (the "Quota and custom CSS" button on their row); the text stays, and they can switch it back on.

Nothing in it may load from elsewhere, so a sheet is refused, with what was not allowed, when it holds `url()` or any other CSS function outside the set that only computes values (colours, gradients, maths such as `calc()` and `var()`, transforms, filters, shapes, timing, grid and selector functions), a backslash, `@import` or `@font-face`. The check reads strings and comments too, so a `url(` inside a comment is refused as well, and a few harmless functions such as `sqrt()` or `@supports selector()` are not on the list.

If your CSS makes the page hard to use, add `?nocustomcss=1` to the address (for example `https://music.example.com/?nocustomcss=1`) and load it. Your CSS and the site CSS stay off until the next full reload, so you can fix yours, turn it off or clear it.

For example, square corners and a softer muted text colour:

```css
:root, [data-theme] {
  --radius-box: 0 !important;
  --radius-field: 0 !important;
  --dn-muted: #8a8f8a !important;
}
```

## Updating and image tags

Update like any other container:

```bash
docker compose pull
docker compose up -d
```

Before a new version changes the database schema, DroppedNeedle writes a verified backup of the database. If an update goes wrong, see [Going back after a failed update](backups-and-restore.md#going-back-after-a-failed-update).

A `:dev` tag (`droppedneedle/droppedneedle:dev`) is built from `main` after CI passes on a push that changes the code, and may still break. Pin a commit with `:dev-<short-sha>`.

Pre-releases are published to the same image under their own version tag, for example `droppedneedle/droppedneedle:v3.1.0-rc.1`. They never move `:latest`, so you only get one by asking for its tag. `:beta` always points at the newest pre-release, so testers can stay on `droppedneedle/droppedneedle:beta` and get each new one with a pull. A full release does not move it: once the version you were testing is released, switch back to `:latest`.

## Running without Docker

Each release on GitHub carries Linux builds, for a machine or container without Docker:

| File | What it is |
|-|-|
| `droppedneedle-<version>-x86_64-linux.tar.gz` | The server (`droppedneedle`) and `droppedneedle-tool`, for 64-bit Intel and AMD |
| `droppedneedle-<version>-aarch64-linux.tar.gz` | The same, for 64-bit ARM |
| `droppedneedle-frontend-<version>.tar.gz` | The web UI, in `static/` |
| `SHA256SUMS` | Checksums of the three |

The files are attached once the image is published, up to an hour after the release appears. `SHA256SUMS` goes up last, so once it is there the set is complete.

They are the binaries the image ships, built on Debian 12, so they need glibc 2.36 or newer (Debian 12, Ubuntu 24.04 or later). Install `ffmpeg` for transcoding and `python3` for Python plugins, both on `PATH`.

Set these before starting `droppedneedle`:

- `ROOT_APP_DIR` to a folder the server's account can write. `config`, `cache`, `plugins` and `imports` go inside it, in place of `/app` in the tables above.
- `DROPPEDNEEDLE_STATIC_DIR` to the `static` folder from the web UI archive. The server copies it into the cache at each start, so it can sit where the server's account cannot write.
- `COMMIT_TAG` to the release's version, such as `v3.2.0`. The About page and the update check read it: unset, the server calls itself a dev build and always offers an update.
- `TZ` to your time zone. Unset, scans run in the system's zone (`/etc/localtime`).

Keep `droppedneedle-tool` in the same folder as `droppedneedle`: the v2 upgrade page runs it from there.

Run it as an ordinary account, never root, and only one server per data folder. `PUID`, `PGID` and `UMASK` belong to the image's entrypoint and do nothing here: files belong to the account that runs the server, under its own umask. Use 027 (`UMask=0027` in a systemd unit) to match the image, so the database and backups stay private. The other variables work as in a container.

To update, stop the server, swap in the new release's binaries and web UI, set `COMMIT_TAG`, and start it. The database backup before a schema change happens the same way. Coming from v2, point `ROOT_APP_DIR` at the folder that holds v2's `config` and `cache` and follow [Upgrading from v2](upgrading-from-v2.md). Where that page runs `droppedneedle-tool` in a container, run the same command directly, as the server's account with the same variables.
