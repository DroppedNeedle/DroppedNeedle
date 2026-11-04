# Signing in apps from the browser

A music app can sign a person in through the browser instead of asking them to create an app-password and type it on their phone. The app opens a DroppedNeedle page, the person signs in if they need to and taps Allow, and the app gets an ordinary app-password named after the app and the device. It shows up under Profile > Connect Apps and can be revoked like any other.

This is the OAuth 2.0 authorization code flow with PKCE for native apps ([RFC 6749](https://www.rfc-editor.org/rfc/rfc6749), [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636), [RFC 8252](https://www.rfc-editor.org/rfc/rfc8252)), so standard app libraries such as AppAuth work with it.

## Contents

- [For admins](#for-admins)
- [For app authors](#for-app-authors)

## For admins

It is off until you list an app. Under Settings > Connect Apps > Sign-in from apps, add each app with:

- **Client ID**: what the app sends as `client_id`. The app's maker tells you this. Up to 64 lower-case letters, digits, `.`, `_` and `-`, starting with a letter or digit.
- **Name**: what the person is shown and what the app-password is called, such as `My Player`. It is never taken from the app's request.
- **Return addresses**: where the browser hands the result back, one per line, matched exactly. Each one is an `https` address or a private scheme named after a domain the app's maker owns, such as `com.example.player:/connect`. The app's maker tells you these too.

Up to eight apps, each with up to four addresses. Empty the list to turn it off again.

The person confirms every time, even when already signed in. Another app on the same phone could claim the same return address and start a sign-in of its own, and the confirmation page, with its warning to allow only after tapping "Sign in" in the app, is what stops that. Allowing needs a normal browser sign-in: a device token cannot do it, just as it cannot create an app-password.

Signing in again from the same device replaces that device's app-password. Taking an app or an address off the list stops new sign-ins; app-passwords it already got keep working until they are revoked.

## For app authors

Ask the server's admin to list your app with your client ID and return addresses. Then:

**1. Open the authorization page** in the system browser (a custom tab, not a web view):

```
https://<server>/connect?response_type=code&client_id=<id>&redirect_uri=<address>
  &code_challenge=<challenge>&code_challenge_method=S256&state=<state>&name=<device>
```

`code_challenge` is the base64url SHA-256 of your verifier (43 characters, no padding); only `S256` is taken. `state` is required, up to 512 bytes, and comes back unchanged. `name` is optional: the device name shown to the person and added to the app-password's name. Other parameters (`scope`, `nonce`) are ignored. With a base path, `/connect` sits under it.

The browser comes back to your return address with `code` and `state`, or with `error=access_denied` when the person cancels, or `error=invalid_request` or `error=unsupported_response_type` for a request that could not be used. An unlisted client or return address is never sent back to.

**2. Trade the code** within two minutes. It works once:

```
POST https://<server>/api/v3/auth/app-connect/token
Content-Type: application/x-www-form-urlencoded

grant_type=authorization_code&code=<code>&redirect_uri=<address>&client_id=<id>&code_verifier=<verifier>
```

Success is `200`:

```json
{ "access_token": "<app-password>", "token_type": "Bearer", "username": "<login name>" }
```

A refusal is `400` with `{"error": "<code>"}`:

- `invalid_request`: the body is not a form, or a parameter is missing or sent twice.
- `unsupported_grant_type`: the grant type is not `authorization_code`.
- `invalid_client`: no app with this client ID is listed (also when sign-in from apps is off).
- `invalid_grant`: the code is unknown, used or expired, or does not match the client ID, return address or verifier. This uses up the code.

The other refusals are checked before the code is looked at, so they leave it to expire after two minutes.

Trades are rate limited per address (`429` with `Retry-After`).

**3. Use the access token as an app-password.** It does not expire. In Subsonic apps send it as `apiKey`, or as the password next to `username` (`u` with `p`, or with `t` and `s`). In Jellyfin apps sign in with `username` and it as the password. It works only on those APIs, never on `/api/v3`: `token_type` says `Bearer` only because app libraries expect it. An account with no login name gets its user id as `username`; for those, send the token as `apiKey`.

For AppAuth, set the authorization endpoint to `https://<server>/connect` and the token endpoint to `https://<server>/api/v3/auth/app-connect/token`. There is no discovery document.
