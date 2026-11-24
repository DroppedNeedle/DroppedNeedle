# Accounts and sign-in

Who can do what, the ways people sign in, and how to get back in when a password or single sign-on fails. Music apps sign in differently: see [Connect your music app](connect-your-app.md).

## Contents

- [Users and roles](#users-and-roles)
- [Sessions](#sessions)
- [Forgotten passwords and recovery](#forgotten-passwords-and-recovery)
- [Single sign-on (OIDC)](#single-sign-on-oidc)
- [Signing in with Plex or Jellyfin](#signing-in-with-plex-or-jellyfin)
- [Scripts and the API](#scripts-and-the-api)

## Users and roles

| Role | Requests | Admin |
|-|-|-|
| Admin | Requests start immediately, no approval | Everything: users, approvals, all settings |
| Trusted | Requests start immediately, no approval | Library curation: editions, matches, the review queue, drop imports, removing tracks, the Recycle Bin (not emptying it) and upgrades. No settings or users |
| User | Requests wait for admin approval | Nothing admin side |

Only an admin removes whole albums, and, in the review queue, uses **Take out of library**, **Put back in library** and **Look again**.

Asking for an album a User's request already waits for joins that request. If it waits for room in its owner's quota, a trusted user's or admin's ask starts it when the asker has room. If it waits for approval, it keeps waiting until an admin approves it under Requests > Approvals, even when a trusted user or an admin joined.

The first account is always admin. Later accounts are created by an admin or automatically on first Jellyfin, Plex, or OIDC sign-in (all start as User). Plex, Jellyfin and OIDC sign-in are switched on and off in the UI. Username and password sign-in is on unless the server is started with `ALLOW_PASSWORD_LOGIN=false` (see [Configuration](configuration.md)); then the sign-in page offers only the other methods, and passwords cannot be set, changed or recovered. First-admin setup and app passwords still work.

Deleting an account under Settings > Users also deletes its play history.

Any signed-in user can see other users' display names, and nothing else about them, when sharing a playlist. Only names people chose are listed: someone shows up there once they save a display name under Profile, or when an admin typed one other than their username while making their account. A name that came from Jellyfin, Plex or single sign-on is never listed. Someone's private playlist shows to everyone else on the Playlists page as a locked card with its track count and its owner's name, never its name, cover or songs; **Only me** in Share says so.

## Sessions

Sessions last 30 days and end with the account if an admin deletes it. "Sign out everywhere" on Profile > Sessions signs every browser and device out of your account; an admin can do it for anyone from Settings > Users. App passwords are separate and keep working. More in [Signing out everywhere](reverse-proxy-and-security.md#signing-out-everywhere).

## Forgotten passwords and recovery

Forgot a password? An admin makes a one-time recovery code for the account under Settings > Users, and "Forgot password?" on the sign-in page takes it. If no admin can sign in either (for example single sign-on stopped working), make the code on the host. Stop the server first, because the command refuses to run while it is up:

```bash
docker compose stop droppedneedle
docker compose run --rm droppedneedle droppedneedle-tool recovery-code USERNAME
docker compose up -d droppedneedle
```

`droppedneedle` is the service name in your compose file; use yours if it differs.

Without Compose (Unraid, Portainer, Synology, where `docker` needs `sudo` over SSH), stop the container and run the command with `docker run`:

```bash
docker stop droppedneedle
docker run --rm -e PUID=1000 -e PGID=1000 \
  -v /path/to/config:/app/config -v /path/to/cache:/app/cache \
  droppedneedle/droppedneedle:TAG droppedneedle-tool recovery-code USERNAME
docker start droppedneedle
```

Use the server's own values:

- Its PUID and PGID (99 and 100 on Unraid), so the files keep their owner.
- The host folders it mounts at `/app/config` and `/app/cache` (on Unraid, the template's Config and Data paths).
- Any `ROOT_APP_DIR`, `CONFIG_DIR`, `CACHE_DIR` or `LIBRARY_DB_PATH` it sets, each as `-e NAME=value`. A `LIBRARY_DB_PATH` outside `/app/cache` needs its own `-v` as well.
- The image it runs, in place of `droppedneedle/droppedneedle:TAG`: `docker inspect -f '{{.Config.Image}}' droppedneedle` prints it. If you pulled a newer one since and the container has not been recreated, use the id `docker inspect -f '{{.Image}}' droppedneedle` prints instead. A newer image may upgrade the database, and the server then refuses to start on it.

`droppedneedle` is the container's name; use yours if it differs.

The code works once, for 15 minutes: open "Forgot password?" on the sign-in page and enter the username, the code and a new password. `USERNAME` is the DroppedNeedle username, which can differ from the name the account has with an OIDC provider; `droppedneedle-tool recovery-code --list-admins` prints the admin usernames.

An account that only signs in through OIDC, Plex or Jellyfin has no password yet. Signed in, its owner can add one under Profile > Account with **Set a password**. Otherwise the host command says the account has none, and the code then gives it its first password; its other sign-in stays linked. A code made by an admin under Settings > Users is only for accounts that already have a password. It is still simpler to keep one admin with a username and password.

If the server runs with `ALLOW_PASSWORD_LOGIN=false`, the code cannot be used until you start it again with `ALLOW_PASSWORD_LOGIN=true` (or without the variable). Turn it back off once you are in.

The command asks for no password of its own: anyone with access to the machine and its data folders can run it.

## Single sign-on (OIDC)

Any OpenID Connect provider works (Authelia, Keycloak, Authentik, Pocket ID, and others):

1. In your provider, create a client for DroppedNeedle. Set its redirect URI to `https://your-droppedneedle-url/api/v3/auth/oidc/callback`. If you serve DroppedNeedle under a sub-path with `BASE_PATH`, put that in front: `https://example.com/music/api/v3/auth/oidc/callback`.
2. Under Settings > Security, enter the issuer URL, the client ID, the client secret (leave it empty for a public client), and the same redirect URI. Keep `openid` in the scopes; `openid email profile` is the default.
3. Test, switch it on and save. A single sign-on button appears on the login page.

When everyone signs in through your provider, turn on "Go straight to SSO" there as well: the login page then starts single sign-on right away instead of showing the sign-in methods. `/login?local` still shows them, for an admin who can't use single sign-on. Signing out lands on `/login?local`, so the provider doesn't sign you straight back in. A single sign-on that fails when your provider sends you back, or that can't reach the provider, lands on the sign-in page too, with a short message on the single sign-on tab: **Continue with SSO** tries again, and the password tab is still there while password sign-in is on. A session that runs out goes to `/login` and signs in again by itself. "Sign out everywhere" ends DroppedNeedle's sessions, not your provider's, so another device sent to sign in again may go straight back in through single sign-on. The switch does nothing while single sign-on is off.

Things worth knowing:

- Use https for the provider. Plain http only works when the provider runs on the same machine or on your local network (a 10.x, 172.16-31.x, 192.168.x, 100.64-127.x or IPv6 local address, or `localhost`), and that applies to every address in its discovery document too. A v2 setup that used plain http to a public address stops working until you switch the provider to https; the server log and the Test button say so. DroppedNeedle also does not follow redirects from the provider, so enter its final URL.
- The issuer URL is the one your provider lists as `issuer` at `/.well-known/openid-configuration`. DroppedNeedle checks that the two match and refuses to log in if they don't.
- DroppedNeedle checks the signature, audience, expiry and nonce of every login token against the keys your provider publishes, so the server needs to reach your provider directly, not only your browser.
- A sign-in only joins an existing DroppedNeedle account when the provider says the email address is verified. Otherwise it creates a new account.
- The first account on a fresh install becomes the admin, whichever way it signs in.
- Coming from v2? v3 does not answer v2's callback, `/api/v1/auth/oidc/callback`. Add the v3 address at your provider before you upgrade, or as you upgrade if it allows only one. The upgrade stops until you have, and changes the address under Settings > Security for you when it ends in v2's path; otherwise enter the new one there after the upgrade.

## Signing in with Plex or Jellyfin

Turn on **Allow login with Plex** on the Plex page, or both **Use Jellyfin** and **Sign in with Jellyfin** on the Jellyfin page, whose URL must be an http or https address. The login page then shows the matching tab, and the sign-in only works while the switch is on.

- Jellyfin checks the username and password against your Jellyfin server.
- Plex sends the user to plex.tv to approve the sign-in. When a Plex server is set up, only accounts that can reach that server get in, and if DroppedNeedle cannot reach the server to check, nobody gets in until it can.
- The check uses the Plex URL on the Plex settings page even while the Plex integration itself is switched off. Clear the URL if you want Plex login without the server check.
- Careful: with "Allow login with Plex" on and no Plex server set up, anyone with a plex.tv account can create an account on your DroppedNeedle. Set up the server first, or keep the switch off.

Either way the user's own media account is linked for playback, so plays count for them without extra setup. Admins can also pre-create accounts for everyone on the media server from Settings > Users > Import; those people then sign in with Plex or Jellyfin and land in their account.

## Scripts and the API

The OpenAPI document is served at `/openapi.json` on your instance. Apart from sign-in, setup and the OAuth callbacks, every `/api/v3/*` route takes a Bearer token or the session cookie; the Wrapped API takes its own key. Everything under `/api/v3/settings/*` needs Admin, except reading the Connect Apps settings, and `/health` stays public for the container check.

### Tokens for scripts

A script or command-line tool should keep a token, not the account's password. It sends the token in an `Authorization: Bearer <token>` header. There are two ways to get one:

- Sign in with `POST /api/v3/auth/login`, sending `"transport": "bearer"` with the username and password. The answer's `token` field holds the token. Leave the transport out and the server sets a cookie and returns no token. With `ALLOW_PASSWORD_LOGIN=false` the login route answers 403. An account that signs in only through Jellyfin sends its Jellyfin username and password to `POST /api/v3/auth/jellyfin/login` instead, with the same `transport` field, while Jellyfin sign-in is switched on.
- From a signed-in session, `POST /api/v3/auth/device-sessions` with a `label` of 1 to 80 characters that names the script mints a token for it. The answer's `token` field holds the token. Minting a label that is already in use replaces the old token, which stops working at once. A device token cannot mint another one. The web UI has no screen for this yet.

Sign in once from a shell (this needs `jq`). The password is read from a prompt, not written into the script:

```bash
printf "Password: "; read -rs DN_PASSWORD; echo; export DN_PASSWORD
TOKEN=$(jq -nc '{username: "alice", password: $ENV.DN_PASSWORD, transport: "bearer"}' \
  | curl -fsS https://music.example.com/api/v3/auth/login \
      -H "Content-Type: application/json" -d @- \
  | jq -r .token)
unset DN_PASSWORD
```

A token lasts 30 days from when it was issued, and there is no refresh. After that the script signs in again, or a new device token is minted. The server keeps only a hash of each token, so a lost token cannot be shown again. Every token shows in Profile > Sessions, and "Sign out everywhere" ends them all. App passwords and app-connect tokens work only on the Subsonic and Jellyfin APIs, never on `/api/v3`.

### Wrapped API

The Wrapped API lets an outside service, such as a newsletterr instance, read your users' year in review. An admin sets it up under Settings > Wrapped API: press "Generate new key", save, and paste the same key into the other service. While no key is saved, every Wrapped request is refused.

The service sends the key in an `X-Wrapped-API-Key` header and needs no account. It can read:

| Route | What it answers |
|-|-|
| `GET /api/v3/wrapped/users` | Every user's id and display name, their email address when one is set, and whether they linked ListenBrainz |
| `GET /api/v3/wrapped/user/{user_id}` | One user's year so far: their top 10 artists, songs, albums and genres, how many songs they loved (counted up to 100) and an estimate of their listens |
| `GET /api/v3/wrapped/server` | A leaderboard of those estimates for every user with data, and ListenBrainz's top artist and album across all its listeners this year |

The numbers come from each user's own ListenBrainz account, linked under Profile > Scrobbling & Discovery. A user with no ListenBrainz link, or an id that matches no user, gets an empty answer (`has_data` false), not an error, and so does everyone while "ListenBrainz recommendations" is off under Settings > ListenBrainz. The listen estimate adds up the top artists only, not every play.

Anyone with the key can read every user's name and email address, so keep it like a password. Generating and saving a new key shuts the old one out at once.
