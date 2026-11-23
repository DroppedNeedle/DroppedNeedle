# Signing in apps from the browser

A music app can sign a person in through the browser instead of asking them to create an app-password and type it on their phone. The app opens a DroppedNeedle page, the person signs in if they need to and taps Allow, and the app receives the credential the admin approved for it. Subsonic and Jellyfin clients receive an app password. Clients that use the DroppedNeedle API can receive a device session.

This is the OAuth 2.0 authorization code flow with PKCE for native apps ([RFC 6749](https://www.rfc-editor.org/rfc/rfc6749), [RFC 7636](https://www.rfc-editor.org/rfc/rfc7636), [RFC 8252](https://www.rfc-editor.org/rfc/rfc8252)), so standard app libraries such as AppAuth work with it.

## Contents

- [For admins](#for-admins)
- [For app authors](#for-app-authors)

## For admins

It is off until you list an app. Under Settings > Connect Apps > Sign-in from apps, add each app with:

- **Client ID**: what the app sends as `client_id`. The app's maker tells you this. Up to 64 lower-case letters, digits, `.`, `_` and `-`, starting with a letter or digit.
- **Name**: what the person sees at sign-in and in their list of connected apps or sessions, such as `My Player`. It is never taken from the app's request.
- **Return addresses**: where the browser hands the result back, one per line, matched exactly. Each one is an `https` address or a private scheme named after a domain the app's maker owns, such as `com.example.player:/connect`. The app's maker tells you these too.
- **Access type**: choose an app password for a Subsonic or Jellyfin app (the default for existing registrations), or a device session for an app that uses the DroppedNeedle API. A device session can act with the signed-in user's permissions, including administrator permissions for an admin. Enable it only for apps you trust with that level of access.

For versions of the independent OtoHari player that support App Connect, choose **Enable OtoHari** to register its client ID and return address. Use **Add app** for other clients.

Registration is per DroppedNeedle instance. Everyone on the instance can then sign in to the registered app using their usual browser login. An instance using OIDC continues to use its existing provider configuration. If a reverse proxy also requires browser sign-in, the app still needs to reach the token endpoint and the API from outside the browser. A browser-only proxy session does not travel with the app.

An instance can register up to eight apps with four return addresses each. Removing every registration turns off new app sign-ins. Each account can have up to 25 active device sessions issued through App Connect.

The person confirms every time, even when already signed in. A private return scheme can be claimed by another app on the same phone. Only tap Allow after starting sign-in in the intended app. App authors can use a platform-verified HTTPS return address to bind the callback to their app. Allowing needs a normal browser sign-in: a device token cannot do it, just as it cannot create an app-password.

Signing in again with the same account, app and device name replaces that device's credential. Removing an app or a return address stops new sign-ins. To end existing access, revoke app passwords under Profile > Connect Apps and device sessions under Profile > Sessions.

## For app authors

Ask the server's admin to list your app with your client ID and return addresses. Then:

**1. Open the authorization page** in the system browser (a custom tab, not a web view):

```
https://<server>/connect?response_type=code&client_id=<id>&redirect_uri=<address>
  &code_challenge=<challenge>&code_challenge_method=S256&state=<state>&name=<device>
```

`code_challenge` is the base64url SHA-256 of your verifier (43 characters, no padding); only `S256` is taken. `state` is required, up to 512 bytes, and comes back unchanged. `name` is optional: the device name shown to the person and included in the credential's label. Other parameters (`scope`, `nonce`) are ignored. With a base path, both the authorization page and token endpoint sit under it. For a device session, reuse a stable, recognisable device name on each sign-in. Two installs using the same account, client ID and device name replace each other’s session. The app and device names together, including the separator, can use up to 80 characters.

Generate a fresh verifier and unpredictable state for each attempt. Keep them with the server address until the callback arrives. Verify the returned state before sending the code and verifier to that same server.

The browser comes back to your return address with `code` and `state`, or with `error=access_denied` when the person cancels, or `error=invalid_request` or `error=unsupported_response_type` for a request that could not be used. An unlisted client or return address is never sent back to.

**2. Trade the code** within two minutes. It works once:

```
POST https://<server>/api/v3/auth/app-connect/token
Content-Type: application/x-www-form-urlencoded

grant_type=authorization_code&code=<code>&redirect_uri=<address>&client_id=<id>&code_verifier=<verifier>
```

An app-password registration receives a `200` response like this:

```json
{ "access_token": "<app-password>", "token_type": "Bearer", "username": "<login name>" }
```

A DroppedNeedle API registration receives a bearer token with an expiry in seconds. For example:

```json
{ "access_token": "<device-session>", "token_type": "Bearer", "username": "<login name>", "credential_kind": "device_session", "expires_in": 2592000 }
```

A refusal is `400` with `{"error": "<code>"}`:

- `invalid_request`: the body is not a form, or a parameter is missing or sent twice.
- `unsupported_grant_type`: the grant type is not `authorization_code`.
- `invalid_client`: no app with this client ID is listed (also when sign-in from apps is off).
- `invalid_grant`: the code is unknown, used or expired, or does not match the client ID, return address or verifier. This uses up the code.

The other refusals are checked before the code is looked at, so they leave it to expire after two minutes.

Trades are rate limited per address (`429` with `Retry-After`).

**3. Use the credential for the registered API.** A native API client should check that the response contains `credential_kind: device_session` before saving the token. If the field is absent, the admin registered the client for an app password. For a device session, send `Authorization: Bearer <access_token>` on requests to the same server’s `/api/v3`. Store it in the platform credential store. Start a new browser sign-in when it expires or is revoked. A device session cannot authorize another app.

For an app password, the credential does not expire. In Subsonic apps send it as `apiKey`, or as the password next to `username` (`u` with `p`, or with `t` and `s`). In Jellyfin apps sign in with `username` and it as the password. It works only on those APIs, never on `/api/v3`: `token_type` says `Bearer` only because app libraries expect it. An account with no login name gets its user id as `username`; for those, send the token as `apiKey`.

For AppAuth, set the authorization endpoint to `https://<server>/connect` and the token endpoint to `https://<server>/api/v3/auth/app-connect/token`. There is no discovery document.

For scripts and manually issued device tokens, see [Tokens for scripts](accounts-and-sign-in.md#tokens-for-scripts).
