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
| Trusted | Requests start immediately, no approval | Library curation: editions, matches, the review queue, drop imports, removals and upgrades. No settings or users |
| User | Requests wait for admin approval | Nothing admin side |

The first account is always admin. Later accounts are created by an admin or automatically on first Jellyfin, Plex, or OIDC sign-in (all start as User). Every login method is switched on and off in the UI; no environment variables involved.

## Sessions

Sessions last 30 days and end with the account if an admin deletes it. "Sign out everywhere" on Profile > Sessions signs every browser and device out of your account; an admin can do it for anyone from Settings > Users. App passwords are separate and keep working. More in [Signing out everywhere](reverse-proxy-and-security.md#signing-out-everywhere).

## Forgotten passwords and recovery

Forgot a password? An admin makes a one-time recovery code for the account under Settings > Users, and "Forgot password?" on the sign-in page takes it. If no admin can sign in either (for example single sign-on stopped working), make the code on the host. Stop the server first, because the command refuses to run while it is up:

```bash
docker compose stop droppedneedle
docker compose run --rm droppedneedle droppedneedle-tool recovery-code USERNAME
docker compose up -d droppedneedle
```

`droppedneedle` is the service name in your compose file; use yours if it differs. The code works once, for 15 minutes: open "Forgot password?" on the sign-in page and enter the username, the code and a new password. `USERNAME` is the DroppedNeedle username, which can differ from the name the account has with an OIDC provider; `droppedneedle-tool recovery-code --list-admins` prints the admin usernames.

An account that only signs in through OIDC, Plex or Jellyfin has no password yet. The command says so, and the code then gives the account its first password; its other sign-in stays linked. Only the host command can do that: a code made by an admin under Settings > Users is for accounts that already have a password. It is still simpler to keep one admin with a username and password.

The command asks for no password of its own: anyone with access to the machine and its data folders can run it.

## Single sign-on (OIDC)

Any OpenID Connect provider works (Authelia, Keycloak, Authentik, Pocket ID, and others):

1. In your provider, create a client for DroppedNeedle. Set its redirect URI to `https://your-droppedneedle-url/api/v3/auth/oidc/callback`. If you serve DroppedNeedle under a sub-path with `BASE_PATH`, put that in front: `https://example.com/music/api/v3/auth/oidc/callback`.
2. Under Settings > Security, enter the issuer URL, the client ID, the client secret (leave it empty for a public client), and the same redirect URI. Keep `openid` in the scopes; `openid email profile` is the default.
3. Test, switch it on and save. A single sign-on button appears on the login page.

When everyone signs in through your provider, turn on "Go straight to SSO" there as well: the login page then starts single sign-on right away instead of showing the sign-in methods. `/login?local` still shows them, for an admin who can't use single sign-on. It does nothing while single sign-on is off.

Things worth knowing:

- Use https for the provider. Plain http only works when the provider runs on the same machine or on your local network (a 10.x, 172.16-31.x, 192.168.x, 100.64-127.x or IPv6 local address, or `localhost`), and that applies to every address in its discovery document too. A v2 setup that used plain http to a public address stops working until you switch the provider to https; the server log and the Test button say so. DroppedNeedle also does not follow redirects from the provider, so enter its final URL.
- The issuer URL is the one your provider lists as `issuer` at `/.well-known/openid-configuration`. DroppedNeedle checks that the two match and refuses to log in if they don't.
- DroppedNeedle checks the signature, audience, expiry and nonce of every login token against the keys your provider publishes, so the server needs to reach your provider directly, not only your browser.
- A sign-in only joins an existing DroppedNeedle account when the provider says the email address is verified. Otherwise it creates a new account.
- The first account on a fresh install becomes the admin, whichever way it signs in.
- Coming from v2? v3 does not answer v2's callback, `/api/v1/auth/oidc/callback`. Add the v3 address at your provider before you upgrade, or as you upgrade if it allows only one. The upgrade stops until you have, and changes the address under Settings > Security for you when it ends in v2's path; otherwise enter the new one there after the upgrade.

## Signing in with Plex or Jellyfin

Turn on "Allow login with Plex" on the Plex page, or "Sign in with Jellyfin" on the Jellyfin page. The login page then shows the matching tab, and the sign-in only works while the switch is on.

- Jellyfin checks the username and password against your Jellyfin server.
- Plex sends the user to plex.tv to approve the sign-in. When a Plex server is set up, only accounts that can reach that server get in, and if DroppedNeedle cannot reach the server to check, nobody gets in until it can.
- The check uses the Plex URL on the Plex settings page even while the Plex integration itself is switched off. Clear the URL if you want Plex login without the server check.
- Careful: with "Allow login with Plex" on and no Plex server set up, anyone with a plex.tv account can create an account on your DroppedNeedle. Set up the server first, or keep the switch off.

Either way the user's own media account is linked for playback, so plays count for them without extra setup. Admins can also pre-create accounts for everyone on the media server from Settings > Users > Import; those people then sign in with Plex or Jellyfin and land in their account.

## Scripts and the API

The OpenAPI document is served at `/openapi.json` on your instance. Apart from sign-in, setup and the OAuth callbacks, every `/api/v3/*` route takes a Bearer token or the session cookie; the Wrapped API takes its own key. Everything under `/api/v3/settings/*` needs Admin, except reading the Connect Apps settings, and `/health` stays public for the container check.
