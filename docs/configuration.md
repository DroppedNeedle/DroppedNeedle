# Configuration

Everything user-editable lives in the web UI and is stored in the database, so a database backup holds it. The environment is only for container basics. This page lists the environment variables, the folders to persist, where each setting lives in the UI, and how to update.

## Contents

- [Environment variables](#environment-variables)
- [Network and reverse proxy](#network-and-reverse-proxy)
- [Folders to persist](#folders-to-persist)
- [Storage and permissions](#storage-and-permissions)
- [Where settings live](#where-settings-live)
- [Updating and image tags](#updating-and-image-tags)

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
| `TZ` | `UTC` | Container time zone, as an IANA name such as `Europe/London`. Daily library scans run at their set time in this zone; a name the server does not know runs them in UTC, and the scan settings say so |
| `SLSKD_DOWNLOADS_PATH` | `/data/downloads/slskd` | Exact in-container path to slskd completions (the compose example uses `/data/slskd/complete`) |
| `LOG_LEVEL` | `INFO` | `DEBUG`, `INFO`, `WARNING` or `ERROR`. The audio decoders log only errors whatever this says; set `RUST_LOG` to see more |

<details>
<summary>Rarely needed variables</summary>

| Variable | Default | What it is |
|-|-|-|
| `CONTACT_EMAIL` | `contact@droppedneedle.com` | Contact address in the outbound User-Agent; MusicBrainz asks for one, so set your own if you run many lookups |
| `DISCOVER_WARMER_ENABLED` | `true` | Keeps each user's Discover page and discover queue fresh in the background while they use them. `false` builds them only when someone opens the page |
| `SHUTDOWN_GRACE_PERIOD` | `10` | Seconds to finish open requests and stop background work on shutdown |
| `HTTP_TIMEOUT` | `30` | Outbound request timeout, seconds |
| `HTTP_CONNECT_TIMEOUT` | `10` | Outbound connect timeout, seconds |
| `HTTP_MAX_KEEPALIVE` | `50` | Idle outbound connections kept per host |
| `RUST_LOG` | unset | Full log filter (e.g. `info,droppedneedle_library=debug`, or `droppedneedle_auth=debug` for sign-in); wins over `LOG_LEVEL` |
| `ROOT_APP_DIR` | `/app` | Base for the cache, database, config and web UI paths below, and for `/app/plugins` and `/app/imports` |
| `CACHE_DIR` | `/app/cache` | Database, backups, served web UI, caches |
| `COVER_CACHE_MAX_SIZE_MB` | `500` | Size cap for downloaded and extracted cover art under `CACHE_DIR/covers`; the least recently shown covers go first |
| `LIBRARY_DB_PATH` | `/app/cache/library.db` | The database file; backups go in `backups/` and Undo's saved copies in `undo/` next to it. `droppedneedle-tool restore` writes a backup back under this file name |
| `CONFIG_DIR` | `/app/config` | Holds `data_enc.key`, the key that seals stored secrets. Replaces `CONFIG_FILE_PATH`, which now stops the start: set `CONFIG_DIR` to that file's folder |
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
| Settings and encryption key | `/app/config` | Persist it |
| Database, backups, cover art and metadata cache | `/app/cache` | Persist it. The `undo` folder beside `library.db` holds the tags, pictures and files Undo and "Restore original state" put back; backups copy it into `backups/undo`, so keep the whole `backups` folder. Provider answers (MusicBrainz, Last.fm and the like) live in `metadata-cache.db`, which survives a restart, is not in the backups, and can be deleted safely while the server is stopped |
| Plugins | `/app/plugins` | Persist it or installs vanish on recreate |
| Download and drop-import staging | `/app/imports` | Optional; without it, staged downloads, large uploads and unmatched files live on the container layer |
| Media | `/data` | Shared parent for library (`/data/music`) and client completions |

Backups are covered in [Backups and restore](backups-and-restore.md).

## Storage and permissions

Keep `UMASK=027` for a private box. Use `002` when DroppedNeedle and another trusted service in the same group both write the same media. Skip `000`: it makes new files writable by every local account the filesystem allows. `UMASK` shapes new files only; a move can keep the source mode the download client set.

Unraid commonly uses `nobody:users` (PUID 99, PGID 100). Point PUID and PGID at whoever owns the mounted config and cache paths. After a PUID change, back to 1000 included, the next start hands the files the old id owns in `/app/config`, `/app/cache`, `/app/plugins` and `/app/imports` to the new one. The container skips ownership changes it cannot make, which covers FUSE, NFS, CIFS, and rootless setups. A read-only config or cache mount refuses to start before anything gets written.

`/app/config` and `/app/cache` must be writable and honor SQLite locking, `fsync`, and atomic replacement. Plain bind mounts, named volumes, Unraid pool or disk paths, and TrueNAS datasets with normal permissions all qualify. The database (`/app/cache/library.db` unless `LIBRARY_DB_PATH` moves it) must be on a local filesystem: DroppedNeedle refuses to start with it on NFS, SMB/CIFS or a FUSE mount, whatever locking the mount offers, or behind a symlink.

On Unraid, keep `/app/cache` on a pool path such as `/mnt/cache/appdata/droppedneedle/cache`, as the template does: a `/mnt/user` share is a FUSE mount unless it is set to exclusive access. An exclusive share works too. If `LIBRARY_DB_PATH` puts the database on a local disk while `/app/cache` is on a network or FUSE mount, or is a symlink, `metadata-cache.db` is kept beside the database instead; if that place is refused too, provider answers are fetched each time and the server log says the provider cache is off. On Docker Desktop for Windows, prefer named volumes for those two paths.

For the media mount, one shared parent (`/data`) with the library and the download client's completions under it keeps imports fast; see [Download clients](download-clients.md#slskd).

## Where settings live

| Setting | Location |
|-|-|
| Library paths, import naming script, scan schedule, folder watching, AcoustID key, storage cap | Settings > Library |
| Library Management profiles, previews, recovery, the recycle bin folder and retention | Library Management (admin), also reached from Organize files under Settings > Library |
| Download clients (slskd, SABnzbd), source priority, quality tiers, picking a copy, wanted watcher, Usenet retention days | Settings > Download Client |
| Newznab indexers or Prowlarr | Settings > Indexers / Prowlarr |
| Edition preferences, managing unconfirmed matches | Settings > Editions |
| Lidarr import | Settings > Lidarr Import |
| Subsonic and Jellyfin APIs, app passwords, transcoding, apps that sign in through the browser | Settings > Connect Apps |
| Jellyfin | Settings > Jellyfin |
| Navidrome | Settings > Navidrome |
| Plex | Settings > Plex |
| Local files (the built-in library) | Settings > Library |
| Last.fm app key (admin, once per instance) | Settings > Last.fm |
| YouTube API key | Settings > YouTube |
| Spotify client ID and secret | Settings > Spotify |
| Ticketmaster and Skiddle keys, sweep scope | Settings > Live Events |
| ListenBrainz connection for scrobbling (admin) | Settings > ListenBrainz |
| Buy links | Settings > Get it |
| Scrobbling and discovery accounts | Profile > Scrobbling & Discovery |
| Release types | Settings > Release Types |
| Home layout | Settings > Home |
| Discover page sections, the discover queue | Settings > Discover |
| Sidebar | Settings > Sidebar |
| Main listening source (ListenBrainz or Last.fm) | Settings > Music Source |
| MusicBrainz server (official or your own mirror) | Settings > MusicBrainz |
| Database backups (admin) | Settings > Backups |
| Users, roles, Jellyfin and Plex user import | Settings > Users |
| Password breach checking, HSTS, the content-security policy, single sign-on (OIDC), who can download library files | Settings > Security |
| Plugins | Settings > Plugins |
| Library images, cache counts and clearing | Settings > Cache |
| Wrapped API | Settings > Wrapped API |
| Outbound provider calls, rate limits and background jobs | Settings > Diagnostics |
| Discover queue and top picks tuning | Settings > Advanced |
| AudioDB images: on or off, key, name search fallback, direct remote images, preferring local cover art | Settings > Advanced |
| Request history, ignored albums and clean-up interval | Settings > Advanced |
| App version, update check and release notes | Settings > About |

## Updating and image tags

Update like any other container:

```bash
docker compose pull
docker compose up -d
```

Before a new version changes the database schema, DroppedNeedle writes a verified backup of the database. If an update goes wrong, see [Going back after a failed update](backups-and-restore.md#going-back-after-a-failed-update).

A `:dev` tag (`droppedneedle/droppedneedle:dev`) is built from `main` on every push that changes the code, and may break. Pin a commit with `:dev-<short-sha>`. Until v3 is released, `main` and `:dev` are still v2.

Pre-releases are published to the same image under their own version tag, for example `droppedneedle/droppedneedle:v3.1.0-rc.1`. They never move `:latest`, so you only get one by asking for its tag. `:beta` always points at the newest pre-release, so testers can stay on `droppedneedle/droppedneedle:beta` and get each new one with a pull. A full release does not move it: once the version you were testing is released, switch back to `:latest`.
