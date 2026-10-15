# Your library

How DroppedNeedle scans your music, works out which album and edition each folder is, and lets you fix its grouping. Scanning and identification never rename or retag a file. Writing tags and filing music is [Library Management](library-management.md).

## Contents

- [Scanning](#scanning)
- [How an album is identified](#how-an-album-is-identified)
- [Unconfirmed matches](#unconfirmed-matches)
- [Album organization](#album-organization)
- [Duplicate artists](#duplicate-artists)
- [Removing albums and tracks](#removing-albums-and-tracks)
- [Downloading from the library](#downloading-from-the-library)
- [Adding a missing album to MusicBrainz](#adding-a-missing-album-to-musicbrainz)

## Scanning

Scans live on the Scanning tab of Library Management. A running scan can be paused, resumed or stopped there; stopping keeps everything it already read. Rescan files rereads every file in the folders you pick, even ones that look unchanged, which helps after you fix files with another tool. Each finished run in Recent runs lists the files it could not read or skipped, each with a short reason and what to do about it (for example, a damaged file says to re-download or replace it). If someone changed the library folders since you opened the page, the scan is refused: reload and try again.

Recent runs can also export a diagnostic report for a scan: a small JSON file with the run's counters, timings, scopes and failed files, to attach to a bug report. Paths are replaced by fingerprints, and it holds no user names, passwords or error text.

MusicBrainz lookups pace at 1 request per second on the official MusicBrainz server. Built-in BrainzMash runs its own local pacing instead. Either way, later scans are incremental, so the first one is the slow one.

## How an album is identified

When most of its files carry a MusicBrainz release ID (Picard and beets write one), DroppedNeedle looks that release up and checks every track against it. Otherwise it searches by album and artist and fetches the best few releases. Once it knows which album it is, it also lists every edition of that album and checks the ones that could hold your files, so the right pressing is weighed even when search missed it. Each file is paired with a track by title, track number and length. When a file's MusicBrainz release ID and recording ID both point to the release but its track ID belongs to another edition (common in downloaded files), the track ID is treated as a tagging mistake, as Picard does: the file is matched by its recording, the edition screen and the file's row on the album page say so, and the right ID is written when the edition's tags are, by writing the edition's tags from the album page or by a Library Management profile that replaces MusicBrainz IDs (both presets do; one that keeps existing values leaves the old ID).

Between editions that fit equally well it looks at the barcode, catalog number, media, country and disc count your tags carry, then at how closely the exact track lengths agree (a remaster or a digital release is often a second or two off the CD), then at your edition preferences.

Fingerprinting stays off until you add an AcoustID API key. With a key, only albums whose tags don't match well get fingerprinted, twenty files per AcoustID request.

Identification has its own Pause button on the Scanning tab of Library Management. The album being identified finishes, nothing new starts until you press Resume, and new scans keep queueing albums in the meantime.

## Unconfirmed matches

DroppedNeedle always picks. A clear match is applied. A close call, a loose match, or a live or compilation release gets the best guess applied too, marked "unconfirmed": pages, playback, other apps and downloads use it, but tagging and organizing leave its files alone until you confirm it (or turn on "Also manage unconfirmed matches" in Settings > Editions). A later scan or a verified whole-album download can confirm it on its own. Only when every candidate is ruled out does the album keep its own tags, marked "no match", with the closest candidates one click away. Library > Unconfirmed matches lists both kinds; nothing waits on you.

A few cases do wait on a person: an undone automatic edition with nothing to go back to, or editions dropped when two albums were combined. Those sit under Unconfirmed matches > Needs a decision. For each one you can use one of the listed releases, keep the album as tagged, take its MusicBrainz match off, close the review without deciding, start identification again, or (admins) take the album out of the library and put it back later. Nothing is deleted from disk.

Filter the list by folder, folder policy, identification state or filing date, tick several reviews (or every match of the filter) to keep them as tagged, use each one's best release, or take them out in one go: you see what will change and what will be left alone before anything is applied. Retry identification on the Library page can be limited to chosen folders. When you approve an edition that names only a release, DroppedNeedle matches the files to it in the background; until that is done the album's files can't be previewed.

## Album organization

When DroppedNeedle groups files into albums wrongly, an admin can fix it from "Album organization" on the album page. Split takes the tracks you tick and makes them a new album (you can name it). Merge folds the whole album into another one you pick. Move puts the tracks you tick on another album. Reset grouping hands an album's tracks back to automatic grouping, which undoes a split, merge or move. Each one shows a preview first: where every track goes, which albums are left empty, and what happens to each album's edition. Nothing changes until you apply, and if the albums changed in the meantime (a scan ran, someone else edited them) or the preview is more than 15 minutes old, you're asked to preview again. These only change how the library groups your files: no file is moved, renamed or retagged. Later scans keep your grouping. An album left empty folds into the one its tracks went to, so its old links, favorites and playlists still work. Player apps that saved the old album or artist find it too.

Editions follow the album they belong to. An album that keeps its tracks keeps its edition. When an album is emptied into another one that has no edition, its edition moves across. When the albums being combined were matched to different editions, you choose whether the receiving album keeps its own or all of them are dropped and the album is identified again; if the receiving album has none and several are on offer, they are all dropped. A dropped edition always comes with a short reason on the album. When an album with an edition someone chose gains tracks, by a merge, a move or a retag DroppedNeedle follows, they are matched to that edition in the background; the edition, who chose it and "leave unmanaged" stay as they were.

After an upgrade from v2, a one-time cleanup tidies album rows the old version left behind: empty album shells are folded into the album they name, and albums that an imported match split away from their siblings in the same folder are put back together. The same cleanup runs after each scan, finished or not, for the albums that scan touched and the albums it left empty, and after identification finishes with an album. It works in short steps and never while a scan runs. Only the catalog changes, never your files.

## Duplicate artists

Duplicate artists can be merged from the artist identity page (admins). The preview counts what moves to the artist you keep: album and track credits, favorites, play history, playlist entries and app IDs. The merged artist's old links lead to the one you keep. As in earlier versions, merging artists can't be undone.

## Removing albums and tracks

Removing an album or a track never deletes your music. The files move into a recycle bin: the recycle bin path in the download settings, or a `.recycle` folder at the top of the library folder the files came from. Each file gets its own dated folder inside it, so you can move it back by hand. Both kinds of bin are emptied of entries older than the recycle retention (30 days by default). Removing an album also stops its failed downloads from retrying, drops its held "couldn't verify" files and its blocklist, and stops its wanted watch unless you untick that in the dialog, in which case it starts looking for a replacement. If any file can't be moved, nothing is removed and the page says why. Rescan on an album page rereads just that album's folders. If an album was excluded from file organization, "Re-enable" on its page lets management touch it again.

## Downloading from the library

You can download a single track or a whole album from the library. A track comes as the original file. An album comes as a zip of its files, untouched and uncompressed, and starts downloading straight away, so even a big box set doesn't keep you waiting while the server packs it. Only files the library has scanned, sitting inside one of your library folders, can be downloaded. By default every signed-in user can download; to keep it to trusted users and admins, or admins only, change "Library downloads" under Settings > Security. Streaming is never limited by this setting. The setting is enforced in the web app and on the download addresses of both player-app APIs (Subsonic `download`, Jellyfin `/Items/{id}/Download`), and a player app is told whether its user may download. A player app that saves music through the same address it plays from is not stopped, because the server cannot tell saving from playing. So in player apps the setting holds for apps that use the download address and honour what they are told. It is not a guarantee.

## Adding a missing album to MusicBrainz

When an album in your library isn't on MusicBrainz yet, an admin or trusted user can add it from the album page with "Contribute to MusicBrainz". DroppedNeedle never edits MusicBrainz for you; it fills in the MusicBrainz release editor and you check and submit it there, under your own MusicBrainz account.

1. Start the contribution. The draft is built from your files: title, artist, tracklist and lengths. Fix anything that's wrong; every value you change is marked as entered by you.
2. Optionally pick the matching Discogs release. Its label, catalogue number, barcode and country can fill the draft, and the Discogs link goes along as a source. Discogs data is only shown for six hours, then you select the release again to refresh it.
3. Run the duplicate check. If MusicBrainz already has the release, link to it instead and you're done. If it only has similar releases, confirm they are different editions.
4. Open the MusicBrainz editor, review and save the release there. MusicBrainz sends you back to DroppedNeedle, which checks the new release in the background (new releases can take a few minutes to show up) and then links the album to it.

If the check can't link the album, the contribution says why and what to do next, and you can retry or enter the release link by hand. If your files change while a contribution is open, rebuild it from the album's current files.

MusicBrainz sends you back to the address your browser used to reach DroppedNeedle, so this works behind a reverse proxy and under `BASE_PATH` without extra setup.
