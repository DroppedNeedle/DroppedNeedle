# Health

Settings > Health lists what is wrong right now, worst first. Admins also see one mark in the header: red ("2 need you") when something is broken, amber ("3 to look at") for warnings and outside services, and nothing when all is well. Other people see only the dot for outside services, as before.

The list is worked out from what the server already knows. It never calls an outside service. Folder checks that do not answer within two seconds are left out of that answer and tried again a minute later, so a dead network share never holds the page.

## Needs you

- **Saved settings don't read.** A stored settings section cannot be opened, usually after the key file changed. The work it controls waits. Open its settings page and save it again.
- **A background job keeps failing.** A loop has failed for ten minutes or stopped. Settings > Diagnostics shows its last error code; the server log has the detail.
- **A download client isn't answering.** It is on and set up, and its last check failed. Check that it runs and that its address is right under Settings > Download clients.
- **slskd's downloads folder can't be used.** The folder is not there or cannot be written. Check the container's mounts.
- **The recycle bin can't be used.** Removals and upgrades wait. Library > Management shows why, and "Use this folder" when you moved the bin on purpose.
- **The import folder can't be used.** It is not there, cannot be written, or overlaps another folder. See Settings > Library.
- **File changes need you.** A tag, cover or file move stopped part way. Library > Management lists them.
- **The data disk is nearly full.** Backups and uploads are refused. Free some space.
- **The nightly backup fails.** It found the database damaged, or has no room. See [Backups and restore](backups-and-restore.md).

## Worth a look

- **Jobs failed today.** Queued jobs that failed for good in the last 24 hours, by kind.
- **A download client is on but not set up**, or Deemix is not signed in to Deezer.
- **A disk is running low.**
- **No nightly backup for two days** while nightly backups are on.
- **Couldn't check.** A folder check (the recycle bin, the import folder, the publish journal, the backups folder) did not answer within two seconds, a disk could not be read, or failed jobs could not be counted, so the list may be missing rows. Rows from the last answer are marked "last known". A dead network share is the usual cause.
- **The database is in memory.** Nothing survives a restart; set `LIBRARY_DB_PATH` to a file.

## For your information

- **An outside service isn't answering.** MusicBrainz, Last.fm and the like. The server retries by itself.
