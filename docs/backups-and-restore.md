# Backups and restore

A backup is a copy of the database, which holds your settings, accounts, playlists and history, plus the undo copies Library Management keeps. This page covers making backups, what to keep with them, and putting one back.

## Contents

- [Making backups](#making-backups)
- [What to keep](#what-to-keep)
- [Restoring a backup](#restoring-a-backup)
- [Going back after a failed update](#going-back-after-a-failed-update)
- [How a restore works](#how-a-restore-works)
- [The key file](#the-key-file)

## Making backups

Admins make and check backups under Settings > Backups: "Back up now" makes one, and "Check" tests whether a backup would restore. DroppedNeedle also makes one by itself, verified, before a new version changes the database schema. The five newest are kept, in the `backups` folder next to `library.db` (`/app/cache/backups` unless `LIBRARY_DB_PATH` moves the database).

Restoring is always done on the host, never from the web UI.

## What to keep

- **The whole `backups` folder**, not only the `.db` files. The copies Undo and "Restore original state" need live in the `undo` folder beside the database, and each backup copies the ones it names into `backups/undo`, which all kept backups share.
- **The key file `config/data_enc.key`.** A backup holds your settings too, and the stored passwords and API keys in it are sealed with this key. A backup cannot be used without it. See [The key file](#the-key-file).

Backups leave out `metadata-cache.db` on purpose: it holds only provider answers that are fetched again when needed.

## Restoring a backup

1. Stop the server:

   ```bash
   docker compose stop droppedneedle
   ```

   Without Compose, `docker stop droppedneedle`.

2. Restore the backup into an empty folder:

   ```bash
   docker compose run --rm droppedneedle droppedneedle-tool restore --backup /app/cache/backups/BACKUP.db --target-dir /app/cache/restored
   ```

   `restore` checks the backup first and never replaces a file. It puts the undo copies the backup names back beside the restored database.

   Without Compose (Unraid, Portainer, Synology, where `docker` needs `sudo` over SSH), run it with `docker run` instead:

   ```bash
   docker run --rm -e PUID=1000 -e PGID=1000 \
     -v /path/to/config:/app/config -v /path/to/cache:/app/cache \
     droppedneedle/droppedneedle:TAG \
     droppedneedle-tool restore --backup /app/cache/backups/BACKUP.db --target-dir /app/cache/restored
   ```

   Use the server's own values:

   - Its PUID and PGID (99 and 100 on Unraid), so the files keep their owner.
   - The host folders it mounts at `/app/config` and `/app/cache` (on Unraid, the template's Config and Data paths).
   - Any `ROOT_APP_DIR`, `CONFIG_DIR`, `CACHE_DIR` or `LIBRARY_DB_PATH` it sets, each as `-e NAME=value`. A `LIBRARY_DB_PATH` outside `/app/cache` needs its own `-v` as well.
   - The image it runs, in place of `droppedneedle/droppedneedle:TAG`: `docker inspect -f '{{.Config.Image}}' droppedneedle` prints it. If you pulled a newer one since and the container has not been recreated, use the id `docker inspect -f '{{.Image}}' droppedneedle` prints instead.

3. Point `LIBRARY_DB_PATH` at the restored database (`/app/cache/restored/library.db`), or move it into place. To move it, run this on the host, in the folder the server mounts at `/app/cache`:

   ```bash
   cd /path/to/cache
   rm -f library.db-wal library.db-shm
   mv restored/library.db library.db
   cp -an restored/undo/. undo/
   ```

   The `-wal` and `-shm` files belong to the old database, so they go first. `cp -an` keeps each undo copy's owner and mode and skips files already there. These names assume the default `LIBRARY_DB_PATH`.

4. Check that `config/data_enc.key` is the key saved with this backup.

5. Start the server again:

   ```bash
   docker compose up -d droppedneedle
   ```

   Without Compose, `docker start droppedneedle`.

`droppedneedle` is the service name in your compose file, or the container's name; use yours if it differs.

## Going back after a failed update

If an update goes wrong, you can put the backup made before it back by hand and run the previous image:

1. Stop the container.
2. Delete `library.db-wal` and `library.db-shm` in `/app/cache` if they exist.
3. Put the backup from `/app/cache/backups` in place of `/app/cache/library.db`, keeping its owner and mode: on the host, `cp -a /path/to/cache/backups/BACKUP.db /path/to/cache/library.db`.
4. Copy everything in `/app/cache/backups/undo` into `/app/cache/undo`, keeping each file's owner and mode and skipping files already there: on the host, `cp -an /path/to/cache/backups/undo/. /path/to/cache/undo/`. Files there are named by their content, so skipping existing ones is safe.
5. Change the image tag back to the previous version and start the container.

`droppedneedle-tool restore` does steps 3 and 4 for you, into an empty folder (see [Restoring a backup](#restoring-a-backup)).

## How a restore works

`droppedneedle-tool restore --backup <file> --target-dir <dir>` writes the database under the name the server uses: `library.db`, or the file name `LIBRARY_DB_PATH` gives, so run it with the same environment as the server. It reads the undo copies from the `undo` folder next to the backup. A copy that is missing there does not stop the restore; it is listed, and Undo then leaves out only the file that needed it.

The target folder must be empty apart from the server's lock files. The restore refuses a folder holding anything it cannot read, saying which folder and why, so fix its permissions or pick a new folder. It never replaces a file: the database takes its name through a hard link, and on a filesystem without hard links (vfat, exFAT) through a rename that checks the name is free first; the report then has a `note` saying so.

A restore that stops before the database is in place leaves its own marker and temporary files, which the next restore into that folder clears; once the database is in place, a later restore into that folder refuses. The server, the host commands and `import` refuse a folder holding that marker but no database, so run the restore again first; a marker left beside a finished database does not stop the server, which logs that it can be deleted.

The restore also refuses a target folder on a network filesystem, as the server does, and a backup whose `-wal` file holds writes the copy would leave out.

## The key file

Put `config/data_enc.key` back beside a restored database. Without it, DroppedNeedle refuses to start on that database, and changes nothing, rather than lose the stored secrets. The database remembers which key it belongs to, so a key file from another instance or another backup is refused as well.

If that key is lost for good, stop the server and run `droppedneedle-tool reset-key --yes`:

```bash
docker compose stop droppedneedle
docker compose run --rm droppedneedle droppedneedle-tool reset-key --yes
docker compose up -d droppedneedle
```

Without Compose (on Synology, with `sudo` over SSH):

```bash
docker stop droppedneedle
docker run --rm -e PUID=1000 -e PGID=1000 \
  -v /path/to/config:/app/config -v /path/to/cache:/app/cache \
  droppedneedle/droppedneedle:TAG droppedneedle-tool reset-key --yes
docker start droppedneedle
```

Use the server's own PUID, PGID, folders and variables, as in [Restoring a backup](#restoring-a-backup), and the image it runs in place of `droppedneedle/droppedneedle:TAG` (`docker inspect -f '{{.Config.Image}}' droppedneedle` prints it; after a pull the container was not recreated for, use the id `docker inspect -f '{{.Image}}' droppedneedle` prints). A newer image may upgrade the database, and the server then refuses to start on it.

It gives the instance a new key and clears every stored password and API key: the secrets in Settings, every app password, and every user's link to Plex, Jellyfin, Navidrome, Spotify, ListenBrainz and Last.fm. It prints the settings it cleared so you know what to enter again.

It replaces a lost key and does not change the Jellyfin ids of what you already have: stored album and artist ids survive the reset, so Finamp and Jellify keep their pins and downloads, and only items added afterwards get ids under the new key. Whoever holds the old key can still work out the picture ids of albums and artists stored before the reset. Subsonic picture, corrections and transcode links made before the reset stop opening. The replaced key is kept as `config/data_enc.key.old` (or `.old.1`, `.old.2` when that name is taken). The command refuses, changing nothing, while the key file still belongs to the database. If `reset-key` is stopped partway, run `reset-key --yes` again to finish it.
