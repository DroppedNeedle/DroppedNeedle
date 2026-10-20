# Requests and downloads

What happens between pressing Request and the album landing in your library, and what you can do when it does not go to plan. Setting up slskd and SABnzbd is in the [README](../README.md#download-sources).

## Contents

- [Which edition is fetched](#which-edition-is-fetched)
- [Single tracks](#single-tracks)
- [Following artists](#following-artists)
- [When a download finishes](#when-a-download-finishes)
- [Import naming script](#import-naming-script)
- [Held files](#held-files)
- [Upgrades](#upgrades)
- [The Downloads page](#the-downloads-page)
- [Choosing a copy](#choosing-a-copy)
- [Blocked sources](#blocked-sources)
- [Drop imports](#drop-imports)

## Which edition is fetched

Downloads follow the edition you chose. If the album is already in your library, a request, an "acquire this edition", or an upgrade fetches the album's edition: the one you picked, else the one that best fits your files. For an album you don't have yet, it fetches the edition the album page showed you, which follows your edition preferences (Settings > Editions). Candidates are ranked against that edition's tracklist: first by how many of its tracks a folder holds, then by how close the track lengths are, then by quality, then by speed.

## Single tracks

A single track is fetched the way you would do it by hand. DroppedNeedle looks up which album the track is on (the edition you chose for it if you own that album, else the release you asked from, else the main official album rather than a compilation or a live set), searches for that album, picks the peer sharing the most complete album folder, and downloads only the track you asked for. The file is then checked against that album's tracklist. A peer sharing just the one song is only used when nobody shares the album with it; when that happens the download records why. On Usenet, which can't download single files, the album release is fetched and only your track is imported.

Every finished download is checked before it touches the library: the files must be readable audio of the right album and edition, inside your quality range, and match a release of the album you asked for track by track. Files that pass are tagged, named by your [import naming script](#import-naming-script) and filed in the library. Files that don't fit well enough are held for you to look at, and bad sources get quarantined so they stop winning.

## Following artists

Follow an artist and DroppedNeedle checks MusicBrainz for their new releases once a day, a few artists at a time and behind your own page loads, so it never crowds MusicBrainz. The first check only remembers what the artist already has. After that, anything new that matches your release types (Settings > Release Types) and that you don't already own shows up under Following > New Releases, with a badge in the sidebar. Turn on auto-download for an artist and new releases are downloaded on their release day: one download per release, even when several people follow the artist, and nothing is fetched if it's already in the library or already downloading. Regular users need an admin to approve auto-download first; trusted users and admins don't. If MusicBrainz can't be reached, the artist is tried again an hour later.

## When a download finishes

DroppedNeedle reads every file, then checks them. Wrong album or edition (a live album or box set when you asked for the studio album), files outside your quality range, sample clips or unreadable files mean that source is skipped and the next one is tried. The rest are matched against the release you asked for, the same way album identification works. If the album is already in your library, its edition (for "Get the missing tracks" the edition of that copy, else the one you picked, else the one that best fits your files) is the only one accepted: the files are checked against its tracklist, files tagged as another edition send DroppedNeedle to the next source, and the missing tracks it asks for come from that edition. A close match goes into your first library folder at the path your import naming script gives it, with the full MusicBrainz tags written, and shows up in the library and on the request straight away, without waiting for a scan. Tracks you already have are not imported twice, and extra files the release doesn't list (bonus tracks, scene extras) are left out.

If some files clearly name other songs, sound like a different recording, or the album as a whole is not close enough, those files are copied to the held area for you to look at, that source is skipped, and the next one is tried, asking only for the tracks still missing. The download settles as held or partly imported only once there are no sources left, or straight away when the only thing in the way is on your side (a file already sitting where a track would go), since another source would not help.

When a download is an upgrade for an album you already have, a new file replaces your copy of a track only if it is better quality; your old file goes to the recycle bin, never straight to deletion, and comes back if the new one can't be placed. The new file joins the album the old one was on. Files that are no better are dropped and your copy stays. A download with thousands of files is not treated as one album: nothing is imported or copied, and the files stay where they are for you to sort out. Each skip and hold is recorded with a short reason and what you can do about it.

Retry import on a failed download imports the files it left on disk right away, with the same checks, without searching or downloading again.

## Import naming script

Downloads and drop imports are filed at the path the import naming script gives them (Settings > Library). It is written in the same naming script language as Library Management, and is read from the tags DroppedNeedle writes, so a Library Management profile with the same script and default path settings never moves an imported file. The default files a track as `Artist/Album (Year)/0103 Title.flac`, with the year left out when the release has none:

```
{albumartist}/{album}{conditional(is_empty(slice(default(original_date, date), 0, 4)), "", concat(" (", slice(default(original_date, date), 0, 4), ")"))}/{disc:02d}{track:02d} {title}.{ext}
```

Text outside braces is kept. `{name}` is a value, such as `albumartist`, `album`, `title`, `track`, `disc`, `ext`, `initial` (the album artist's first letter, past a leading "The") or any tag name, and `{track:02d}` pads a number. Anything else in braces is an expression, such as `default(a, b)`, `conditional(test, then, else)`, `is_empty(x)`, `concat(...)` or `slice(text, start, end)`. Characters a file name can't hold become `_`, and each folder and file name is cut to 240 bytes, keeping the extension. Saving checks the script and shows where it would file an example track. A script that leaves a folder name empty for some album stops that import with the reason "The import naming script gave no usable path": fix the script, then retry the import.

## Held files

Held files wait on the Downloads page and on their album's page. Each one says why it was held and what to do. You can play it to hear whether it's the right song, import it as it is, or discard it (the file is deleted and the album's automatic retry picks up again). Files held because AcoustID heard a different recording have a Re-check button that runs the fingerprint again and imports the file if AcoustID now agrees. An imported file goes into the edition you chose for the album; if the track isn't on that edition, the page tells you to pick the edition it belongs to first. When importing the last held files makes an album whole, its download shows as completed. Admins can also import or discard, in one go, every file of a download that only waited on the library side (for example a file already sitting where a track would go).

## Upgrades

With upgrades switched on in the download settings, admins and trusted users see the albums whose worst file is below your quality cutoff and can ask for a better copy of an album or a single track. Asking for one that already meets the cutoff queues nothing. The background upgrade scan, when switched on, works through the same list on its own, under the oldest admin account.

## The Downloads page

The Downloads page updates on its own: progress, new tasks and status changes arrive over the same live connection as the rest of the app, so nothing needs a reload. Every download that failed, was held or landed short shows one short reason and what you can do about it, taken from what the import decided (for example "The files belong to a different album by the same artist"), never a raw error.

On the Downloads page you can cancel a download, retry it, or move a Soulseek download that sits in a slow peer's queue on to the next source. Cancel on a failed download stops its automatic retries and its wanted watch. Retry starts a fresh attempt straight away; for an album it also clears that album's blocked sources, so a source that failed before gets another chance. The bulk buttons clear finished downloads, stop every scheduled retry, or retry every failure that won't retry by itself. Admins see and act on everyone's downloads; everyone else sees their own.

## Choosing a copy

To choose the copy yourself, press "Pick a source" on a failed, cancelled or partial album download. DroppedNeedle asks every download source you have turned on for that album and lists what each one has, grouped by source with the best fit first. Soulseek folders are compared with the tracklist of the edition you asked for, so each one says plainly what it looks like ("Holds all 12 tracks of this edition", "Holds 9 of the 12 tracks", "2 track lengths do not match"). Pick one and DroppedNeedle downloads exactly that folder or release, then checks the files like any other download; if it fails, the normal search takes over. "None of these - keep watching" closes the search and puts the album on your wanted list, and the copies you turned down are remembered so they aren't offered as new finds. A search takes up to a couple of minutes, and the results update by themselves when it finishes. If no source is turned on or none of them answers, the search says so and what to check.

Before downloading an album on its own, DroppedNeedle looks at what every source has, the same way "Pick a source" does. If a copy looks like the whole edition in a quality you accept, it downloads that one. If none does, the album waits in the download queue as "Awaiting review" with the copies it found, and you choose one, or press "None of these - keep watching". Nothing is downloaded until you do. You can change this under Settings > Download Client, "Picking a copy": wait only when no copy looks right (the default), always wait for your pick, or never wait and take the best copy found. Single tracks are not held this way; they are fetched from their album's folder as described above.

## Blocked sources

Admins can see blocked sources (Downloads > Quarantine) with the reason each was blocked, and remove one so it can be tried again. Blocks expire on their own after a week.

## Drop imports

Bought music somewhere else? Drop it in. Admins and trusted users can upload up to 25 files at once on the downloads page, 8 GB in all: audio files, or `.zip` archives straight from the store. The drop zone shows how much has been sent. If nothing more gets through for a minute, the upload stops with an error. DroppedNeedle unpacks the archives (audio only), treats each top-level folder as one album, and works out which album it is the way a library scan would: MusicBrainz tags first, then the album and artist names (from the tags, or a folder named `Artist - Album`), then AcoustID fingerprints when you have set an AcoustID key.

- A folder that matches closely is renamed with your import naming script, tagged, and added to the library. If the album is now complete, its open request and wanted watch are settled and everyone who asked for it is told.
- If your library already keeps an edition of that album, the files must match that edition. Files of another edition wait for review instead of quietly importing as a second edition.
- Tracks you already have are skipped, unless the upload is better quality and clearly the same song: its tags must match the track closely, and with an AcoustID key set the audio must not be a different recording (after a hand match, AcoustID has to confirm it). Then it replaces your copy and the old file goes to the recycle bin; if the new file can't be placed, the old one is put back. A file that fails these checks waits for review with the reason.
- Extra files that came with the album but aren't on its tracklist (a bonus track MusicBrainz doesn't list) are kept alongside the album under their own title and number. Files whose tags name a track they don't match wait for review on their own.
- Anything DroppedNeedle can't place waits for review with a plain reason and what to do. Pick the album yourself with Match, or Discard to delete the files. Review items are kept for 90 days.

If the server restarts in the middle of an import, that import is marked as interrupted; drop the files in again.

The server log notes each upload when it starts and when it has arrived (its size and file count), and each import when it starts and finishes.

### Behind a reverse proxy

A proxy in front of DroppedNeedle has its own upload limits, and a large zip can hit them before DroppedNeedle sees it. An upload the proxy refuses shows an error naming the proxy; one it holds stalls and stops after a minute.

- nginx accepts 1 MB by default. Set `client_max_body_size 8g;` (or `0` for no limit) in the DroppedNeedle `location`, and raise `client_body_timeout`, `proxy_send_timeout` and `proxy_read_timeout` if your uplink is slow. `proxy_request_buffering off;` passes the upload straight through instead of storing it on the proxy first.
- Cloudflare's proxy takes at most 100 MB per request on the Free and Pro plans. Upload larger zips from your own network, at DroppedNeedle's own address, or split the album into smaller zips.
