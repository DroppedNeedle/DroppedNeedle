# DroppedNeedle docs

## Set up

- [Configuration](configuration.md): environment variables, folders to persist, permissions, where each setting lives, updating, running without Docker.
- [Download clients](download-clients.md): setting up slskd, SABnzbd or NZBGet with Newznab indexers or Prowlarr, Deemix for Deezer, and drop imports.
- [Bringing in an existing library](downloads.md#bringing-in-an-existing-library): the import folder, for a collection DroppedNeedle should file for you.
- [Accounts and sign-in](accounts-and-sign-in.md): roles, single sign-on, Plex and Jellyfin sign-in, password recovery.
- [Reverse proxy and security](reverse-proxy-and-security.md): proxy headers, base path, public address, HSTS, content-security policy.
- [MusicBrainz sources and self-hosted mirrors](musicbrainz-mirror-selfhosting.md): which MusicBrainz server to use, and running your own.

## Use

- [Features](features.md): everything DroppedNeedle does, one line each.
- [Your library](library.md): scans, formats, identification, unconfirmed matches, album organization, lyrics, ratings.
- [Library Management](library-management.md): profiles, tags, artwork, previews and Apply, undo, editions.
- [Requests and downloads](downloads.md): editions and single tracks, following artists, held files, upgrades, choosing a copy.
- [Your year in review](year-in-review.md): what the page shows, what counts, and who sees it.
- [Lists on rotation](playlists-on-rotation.md): the five playlists made from your library, how they change, keeping and copying one.
- [Discovery](discovery.md): Home, Discover, radio, Weekly Mix, covers, Last.fm, ListenBrainz, Spotify import, music apps in depth.
- [Install as an app](install-as-an-app.md): the web UI on a phone, tablet or computer, with its own icon and window.
- [Connect your music app](connect-your-app.md): what to enter in Symfonium, Finamp, Feishin and other Subsonic or Jellyfin apps.
- [Custom CSS](custom-css.md): restyling the app, and the style hooks that stay stable across releases.
- [Troubleshooting](troubleshooting.md): common problems and their fixes.
- [Health](health.md): what Settings > Health lists and how to fix each problem.

## Run and extend

- [Backups and restore](backups-and-restore.md): making backups, what to keep, restoring, the key file.
- [Signing in apps from the browser](app-connect.md): letting a music app sign in without a typed app password, for admins and app authors.
- [Plugins](../PLUGINS.md): how plugins run, the manifest and the protocol.
- [Writing a plugin](writing-a-plugin.md): a small plugin from an empty folder to a GitHub release.

## Upgrade

- [Upgrading from v2](upgrading-from-v2.md): the upgrade page and command, what moves across, going back.
- [What changed in v3](v3-changes.md): what v2 did that v3 removed, changed or moved.
- [Upgrading from v2 by hand](upgrading-by-hand.md): export, validate, dry run and import as separate steps.
