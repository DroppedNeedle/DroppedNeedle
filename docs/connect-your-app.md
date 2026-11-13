<!-- Checked against the server's Subsonic and Jellyfin APIs and the Connect Apps pages, not yet against each app. -->

# Connect your music app

DroppedNeedle answers the OpenSubsonic and Jellyfin APIs, so music apps can play your library as if it were a Subsonic or Jellyfin server. This page says what to enter in each app.

## Contents

- [Before you start](#before-you-start)
- [What to enter](#what-to-enter)
- [Each app](#each-app)
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
| Feishin | Subsonic or OpenSubsonic | |
| Arpeggi | Subsonic | |
| Jellify | Jellyfin | |
| Manet | Jellyfin | |

## Signing in from the browser

An app that supports it can sign you in through the browser instead of asking for a typed app password: it opens a DroppedNeedle page, you sign in if you need to and tap Allow, and the app gets an app password of its own, listed under Profile > Connect Apps. This only works for apps an admin has listed under Settings > Connect Apps > Sign-in from apps, with the client ID and return addresses the app's maker gives. See [Signing in apps from the browser](app-connect.md).

## What apps get

Apps list your own playlists only, unless you turn on the switch for other people's public playlists under Profile > Connect Apps. Plays in an app count like plays in the web player, and ratings are shared with Subsonic apps. A track is transcoded only when the app asks for another format or a lower bitrate. The details, including sign-in limits, pictures and ids, are in [Music apps](discovery.md#music-apps).
