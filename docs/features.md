# Features

Everything DroppedNeedle does, one row each, with links to the page that covers it.

| Feature | What you get |
|-|-|
| Request | Whole albums or single tracks from the MusicBrainz catalog, with an approval queue for the User role. See [Requests and downloads](downloads.md) |
| Wanted | Failed and partial requests get re-searched on their own until a verified copy shows up |
| Quality | An ordered list of formats and qualities to try, automatic upgrades when a better copy appears, storage caps and quotas |
| Player | Queue, shuffle, 10-band EQ, lyrics from your files (or from LRCLIB when it is on), live now-playing updates |
| Discovery | Trending, charts, genre browsing, recommendations from your ListenBrainz and Last.fm history, an album-by-album discover queue, and a weekly mix that can queue up to five missing albums. See [Discovery](discovery.md) |
| Live events | Ticketmaster and Skiddle gig alerts for artists you follow, each user with their own cities |
| Following | New-release radar with optional auto-download, release-type filters, and sidebar badges |
| Library | Browse, filter, rate, download, rescan, and remove albums (ratings are shared with Subsonic apps). Search finds your songs as well as artists and albums, and plays a song with one click. Albums DroppedNeedle isn't sure of are listed as unconfirmed, without blocking anything. Admins see artists that appear twice under the same name, or whose songs a tag edit moved to a new name, with the MusicBrainz evidence for each, and can merge them or mark them as different people. See [Your library](library.md) |
| Drop imports | Drag in a zip or loose files from anywhere you buy music; identified, tagged, and filed, or held for a manual match |
| Playlists | Mix Jellyfin, Navidrome, Plex, local, YouTube, and Spotify imports in one place, share read-only with one toggle |
| YouTube links | Find an album or its tracks on YouTube from the album page and play them there. Paste your own links on the YouTube library page. Each search spends one unit of the daily quota set under Settings > YouTube; a saved link never searches again |
| Library Management | Optional MusicBrainz Picard-style tags, artwork, and organization behind an admin preview with dry run and typed confirmation. A file whose tags a write could not keep whole, such as one holding a frame twice, is refused with the reason. Off until you enable it. See [Library Management](library-management.md) |
| Plugins | Add download sources, scrobble targets, purchase links and more from a GitHub repository, pinned to an exact commit. See [PLUGINS.md](../PLUGINS.md) |

## What plugs in

| Direction | What plugs in |
|-|-|
| In | Your slskd, your SABnzbd with your Newznab indexers or Prowlarr, and drop imports. Identity from MusicBrainz and AcoustID, album art from your files and the Cover Art Archive, artist photos from AudioDB, Deezer and Wikimedia Commons, matched by MusicBrainz id |
| Out | Jellyfin, Navidrome, Plex, local files, and YouTube previews, plus OpenSubsonic and Jellyfin APIs so apps like Symfonium, Finamp, Feishin, Amperfy, Arpeggi, Jellify, and Manet can play from you. The apps get the lyrics your files carry. Albums on Plex and Navidrome whose tags carry no MusicBrainz id are found by title and artist |
| Around | ListenBrainz and Last.fm scrobbling, Spotify playlist import, Ticketmaster and Skiddle gigs, Deezer and iTunes preview clips, and purchase links that put Bandcamp first |
