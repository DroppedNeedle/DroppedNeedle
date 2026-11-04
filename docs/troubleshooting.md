# Troubleshooting

Common problems and what fixes them. Messages from the upgrade from v2 are listed in [If something fails](upgrading-from-v2.md#if-something-fails).

## Downloads

- **Downloads finish in the client but never import.** `SLSKD_DOWNLOADS_PATH` must point at the exact completions directory, visible read-write. The Download Client page shows the path status and the reason. Every few minutes DroppedNeedle also checks whether slskd's finished downloads can actually be found in that folder, and if they can't, the page says why: the folder looks empty, or it holds other files (usually because the mount is a parent folder such as your whole media share). It also shows where slskd itself saves, so you can line the two up. A wrong path is usually fixed with the downloads subfolder box on that page. See [the mount rules](download-clients.md#slskd).
- **Separate-mount warning.** Imports still work through copy-and-remove, which briefly needs room for both copies. One shared `/data` parent with no nested binds restores fast moves.
- **Client connection fails or returns 401.** Wrong URL or API key. Re-enter both under Settings > Download Client and Test.
- **Searches return nothing or the network drops you.** slskd needs shared folders and a healthy Soulseek connection. Leechers get banned.

## Library

- **Scan finds nothing or files pile into manual review.** Check the library path is readable. Untagged files with no fingerprint match need a human.

## Web UI and reverse proxy

- **Now playing, scan progress and "download started" pop-ups only change when you reload.** Each open tab keeps one long-lived connection to `/api/v3/events/stream` for live updates, and something in between is holding it back. Behind nginx, add `proxy_buffering off;` and `proxy_read_timeout 1h;` for that path (the app already sends `X-Accel-Buffering: no`). Other proxies have a similar "don't buffer" switch. Don't let the proxy compress `text/event-stream` either.
- **Signed in, but every save answers `403 Origin not allowed`.** That is the reverse proxy, not the account. See [Public address](reverse-proxy-and-security.md#public-address), which ends with `droppedneedle-tool public-address`, the host command for it.
- **The web UI is a blank page and the browser console names the content-security policy.** Usually a reverse proxy that rewrites the page or injects a script, such as Cloudflare's Rocket Loader. Turn that off for DroppedNeedle, or switch the policy to report-only from the host: see [Content-security policy](reverse-proxy-and-security.md#content-security-policy).

## Signing in

- **Nobody can sign in** (a forgotten admin password, or single sign-on stopped working). Make a recovery code on the host: see [Forgotten passwords and recovery](accounts-and-sign-in.md#forgotten-passwords-and-recovery).

## Starting up

- **The server refuses to start with a message about the key file.** A database whose `data_enc.key` is missing or belongs to another database is refused, and nothing is changed. Put back the key saved with it: see [The key file](backups-and-restore.md#the-key-file).
- **The server refuses to start with the database on a network share or a symlink.** The database must be on a local filesystem: see [Storage and permissions](configuration.md#storage-and-permissions).
