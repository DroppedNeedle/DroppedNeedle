# Troubleshooting

Common problems and what fixes them. Messages from the upgrade from v2 are listed in [If something fails](upgrading-from-v2.md#if-something-fails).

## Downloads

- **Downloads finish in the client but never import.** `SLSKD_DOWNLOADS_PATH` must point at the exact completions directory, visible read-write. The Download Client page shows the path status and the reason. Every few minutes DroppedNeedle also checks whether slskd's finished downloads can actually be found in that folder, and if they can't, the page says why: the folder looks empty, or it holds other files (usually because the mount is a parent folder such as your whole media share). It also shows where slskd itself saves, so you can line the two up. A wrong path is usually fixed with the downloads subfolder box on that page. See [the mount rules](download-clients.md#slskd).
- **Separate-mount warning.** Imports still work through copy-and-remove, which briefly needs room for both copies. One shared `/data` parent with no nested binds restores fast moves.
- **Client connection fails or returns 401.** Wrong URL or API key, or slskd's CIDR allowlist for the key leaves out DroppedNeedle's address. Re-enter the URL and key under Settings > Download Client and press **Test connection**.
- **SABnzbd refused the name in this address.** SABnzbd only answers names on its host whitelist. Add the name from the URL (`sabnzbd` on a compose network) under Config > Special in SABnzbd, then test again.
- **Searches return nothing or the network drops you.** slskd needs shared folders and a healthy Soulseek connection. Leechers get banned.

## Library

- **Scan finds nothing or files pile into manual review.** Check the library path is readable. Untagged files with no fingerprint match need a human.

## Web UI and reverse proxy

- **A message says "Internal server error".** It comes with a short ref, such as `4f1c2a9e`. Give that ref to your admin, who can search the server log for it.
- **Now playing, scan progress and "download started" pop-ups only change when you reload.** Each open tab keeps one long-lived connection to `/api/v3/events/stream` for live updates, and something in between is holding it back. Behind nginx, add `proxy_buffering off;` and `proxy_read_timeout 1h;` for that path (the app already sends `X-Accel-Buffering: no`). Other proxies have a similar "don't buffer" switch. Don't let the proxy compress `text/event-stream` either.
- **Signed in, but every save answers `403 Origin not allowed`.** That is the reverse proxy, not the account. See [Public address](reverse-proxy-and-security.md#public-address), which ends with `droppedneedle-tool public-address`, the host command for it.
- **The web UI is a blank page and the browser console names the content-security policy.** Usually a reverse proxy that rewrites the page or injects a script, such as Cloudflare's Rocket Loader. Turn that off for DroppedNeedle, or switch the policy to report-only from the host: see [Content-security policy](reverse-proxy-and-security.md#content-security-policy).

## Signing in

- **Nobody can sign in** (a forgotten admin password, or single sign-on stopped working). Make a recovery code on the host: see [Forgotten passwords and recovery](accounts-and-sign-in.md#forgotten-passwords-and-recovery).

## Disk space

- **The log says free space is running low.** Every five minutes the server reads the free space of each folder it writes: the data folder (the database and its undo copies), config, temp, its imports folder, each library folder, the recycle bin, the download folders and the import folder. It logs one line when a folder runs low and one when it recovers. The data folder is low below twice the size of the database and undo copies plus 1 GB, and nearly full below their size plus 256 MB. Any other folder is low below 2 GB.
- **The data disk is nearly full.** Backups are refused, a new version that backs up before it upgrades does not start, and an upload that would not fit is refused with how much room it needs. Free some space; nothing else needs doing.
- **A message says "The server's disk is full".** A request failed while the data or temp folder had less than 64 MB free, so the server answered `507` with the code `DISK_FULL`. Free some space on that disk, then try again.

## Reporting a problem

- **Recent problems.** Settings > Diagnostics lists the server's latest warnings and errors, newest first; repeats within ten minutes count as one. Type the ref an error showed into the Ref box to find the one that person met. It shows what the server log shows, paths and names included, so it is for admins only.
- **The support bundle.** Settings > Diagnostics > Download support bundle opens one JSON file to read, then Download saves exactly that text. Attach it to a bug report.
  - It holds: the version and build, the machine (architecture, kernel release, container or not, CPUs, memory), each folder by role ("library" with its number) with its filesystem's kind, whether it is local or a network share, and its free and total space, each settings section's state (default, saved or unreadable), the download clients and plugin download sources that are on, background jobs and how long they have failed, queue sizes by job kind, outside services not answering, database and cache file sizes, the schema version, how many backups there are and the newest nightly one's age, counts (accounts by role, artists, albums, tracks, playlists, requests by state, held files), enabled plugins with their versions, and recent problems: their level, module, file and line, counts, times, refs the server made, and a few fields such as error codes.
  - It never holds: settings values, keys, tokens or passwords (masked or not), paths, host names, addresses or ports, user names, emails or ids, what your library holds (names, MusicBrainz ids), search text, message, error or cause text, or request paths. A ref someone typed themselves is counted, not shown. Which deployment variables are set is left out too, because the server does not keep that.

## Starting up

- **The server refuses to start with a message about the key file.** A database whose `data_enc.key` is missing or belongs to another database is refused, and nothing is changed. Put back the key saved with it: see [The key file](backups-and-restore.md#the-key-file).
- **The server refuses to start with the database on a network share or a symlink.** The database must be on a local filesystem: see [Storage and permissions](configuration.md#storage-and-permissions).
- **Docker, Unraid or autoheal says the container is unhealthy.** The health check, `/health`, answers `200` while the server runs. It answers `503` (`SERVER_BUSY`) only when the database stops answering: a read fails, or one write has held the database for two minutes, usually because the disk under it stopped responding. A restart can clear that, and the server log says which it was. An outside service being down, a full disk or a broken setting never fails it. While the server waits for its upgrade from v2, it answers `200` with the status `upgrade`.
