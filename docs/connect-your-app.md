<!-- Checked against the server's Subsonic and Jellyfin APIs and the Connect Apps pages, not yet against each app. -->

# Connect your music app

DroppedNeedle answers the OpenSubsonic and Jellyfin APIs, so music apps can play your library as if it were a Subsonic or Jellyfin server. This page says what to enter in each app.

## Contents

- [Before you start](#before-you-start)
- [What to enter](#what-to-enter)
- [Each app](#each-app)
- [Moving a queue between players](#moving-a-queue-between-players)
- [Signing in from the browser](#signing-in-from-the-browser)
- [What apps get](#what-apps-get)

## Before you start

1. **An admin turns the APIs on.** Both are off until an admin switches them on under Settings > Connect Apps: "OpenSubsonic API" for Subsonic apps, "Jellyfin-compatible API" for Jellyfin apps. Transcoding is set there too, and its "On-the-fly transcoding" switch also lets the web player transcode tracks your browser can't play ([Supported formats](library.md#supported-formats)).
2. **You make an app password.** Under Profile > Connect Apps, give it a name (such as `Symfonium (phone)`) and create it. The password is shown only this once; if you lose it, revoke it and make a new one. Make one per app or device, up to 25, so you can revoke one without signing the others out. Your account password never works in a music app.

## What to enter

Profile > Connect Apps shows the server URL for each API, with a copy button. They are your DroppedNeedle address followed by the API's path:

| Server type in the app | Server URL | Username | Password |
|-|-|-|-|
| Subsonic or OpenSubsonic | `https://music.example.com/subsonic` | Your DroppedNeedle username | The app password |
| Jellyfin | `https://music.example.com/jellyfin` | Your DroppedNeedle username | The app password |

With `BASE_PATH` set, it goes before the API's path: `https://example.com/music/subsonic` and `https://example.com/music/jellyfin`. Use the address the app can reach: the one your reverse proxy serves, or the server's own address and port (`http://192.168.1.5:8688/subsonic`) on your network.

Apps that use OpenSubsonic API keys can send the app password as the key instead of a username and password.

## Each app

| App | Server type to pick | Notes |
|-|-|-|
| Symfonium | OpenSubsonic | Its Jellyfin mode is not tested with DroppedNeedle |
| Finamp | Jellyfin | Transcoded playback uses HLS, which needs transcoding on under Settings > Connect Apps |
| Feishin | OpenSubsonic | Shows word-by-word lyrics, singers, background vocals and translations where your lyrics files carry them |
| Arpeggi | Subsonic | |
| Jellify | Jellyfin | |
| Manet | Jellyfin | |

## Moving a queue between players

Apps that use the native v3 API can offer to move playback between players signed
in to the same DroppedNeedle account. Both players must opt in and stay connected.
The destination receives the local tracks in playback order, the current track,
its elapsed time and whether it was playing. A paused queue stays paused. Each
player streams from DroppedNeedle using its own session.

For app authors, the two playback endpoints use the [v3 API contract](../server/openapi/openapi.json).

1. On opt-in, choose a random UUID for the running player. Call
   `POST /api/v3/playback/handoff/sync` with its ID, a display name and
   `can_send` indicating whether it has a transferable queue. Poll while
   available (e.g. every five seconds). The response lists your account's
   available players and the next command for this player. Send
   `disconnect: true` on opt-out. A player also expires after 45 seconds
   without a heartbeat.
2. To send or pull a queue, `POST /api/v3/playback/handoff` with a fresh
   transfer UUID, this player's ID and the source and destination player IDs.
   Retry this same request with the same UUID after a lost response.
   One transfer can run per account at a time.
3. On `offer`, the source sends an `offered` receipt with its snapshot.
   Include local IDs, display labels and optional duration for up to 2,000 tracks.
   Keep duplicates, mark the current occurrence by its zero-based index, and
   report the position in milliseconds. The source continues playing.
4. On `receive`, restore the queue and seek. Send `accepted` only once
   playback has started or the paused queue is ready. Send `declined` if
   a track is unavailable, playback cannot start or the user cancels.
5. On `release`, the source pauses the offered playback session and replies
   `released`. If the listener has since started something else, leave that
   playback alone. The release command identifies the destination so the source
   can show which player took over. A `failed` command ends the transfer and
   leaves the source alone.

Receipts travel in the next sync call. Until acknowledged, a command may
appear again. Apply each command once per transfer and resend its receipt if
needed. A step expires after 30 seconds. A restart forgets all players and
pending transfers.

## Signing in from the browser

An admin registers the app once under Settings > Connect Apps > Sign-in from apps. In the app, choose browser sign-in, sign in to your DroppedNeedle server as usual, review the access shown and tap Allow. A Subsonic or Jellyfin app gets an app password that you can revoke under Profile > Connect Apps. An app registered for the DroppedNeedle API gets a device session that you can revoke under Profile > Sessions. See [Signing in apps from the browser](app-connect.md).

## What apps get

Apps list your own playlists and those shared with you, plus other people's public playlists if you turn on the switch under Profile > Connect Apps. Plays in an app count like plays in the web player, and ratings are shared with Subsonic apps. A track is transcoded only when the app asks for another format or a lower bitrate. The details, including sign-in limits, pictures and ids, are in [Music apps](discovery.md#music-apps).
