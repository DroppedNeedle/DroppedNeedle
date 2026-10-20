# Reverse proxy and security

Running DroppedNeedle behind a reverse proxy, telling it its public address, and the HSTS and content-security policy settings under Settings > Security.

## Contents

- [Proxy headers](#proxy-headers)
- [Base path](#base-path)
- [Public address](#public-address)
- [Addresses given to player apps](#addresses-given-to-player-apps)
- [HSTS](#hsts)
- [Content-security policy](#content-security-policy)

## Proxy headers

Behind a reverse proxy, list the proxy in `TRUSTED_PROXY_IPS` and have it send `X-Forwarded-Proto` and the `Host` header exactly as the browser sent it, port included (or the same value as `X-Forwarded-Host`). In nginx that is `proxy_set_header Host $http_host;`, not `$host`, which drops the port. The `Secure` cookie, HSTS, per-client rate limits and the addresses that player apps are given (below) all depend on the proxy being listed and sending those headers, except that a sign-in from an `https` address listed under Public address (below) gets a `Secure` cookie either way. In one browser, the `https` address and the plain `http` address on the same host name keep separate sign-ins, and signing out at the `https` address ends both. If the proxy is on a private address and not listed, the log says so once, naming the address to add.

Drop imports upload up to 8 GB in one request, far more than a proxy accepts by default. See [Drop imports behind a reverse proxy](downloads.md#behind-a-reverse-proxy) for the nginx settings and Cloudflare's limit.

## Base path

With `BASE_PATH` set, everything moves under it: the web UI, `/api/v3`, the Subsonic and Jellyfin APIs, and `/health` (the container health check follows it). Point your proxy at the prefix without stripping it.

## Public address

The web UI's changes are only accepted from the address DroppedNeedle believes it is served at. A proxy that drops the port, hides HTTPS or replaces `Host` makes that address wrong: signing in works, and every save answers `403 Origin not allowed`, mostly on plain-HTTP sites and in older browsers. The first time it happens the log names both addresses. If you cannot change the proxy, or saving is still refused, tell DroppedNeedle its address under Settings > Security > Public address. Write it as the browser's address bar shows it, up to the host and port: `https://music.example.com` or `http://192.168.1.5:8688`, with no path, not even the base path. You can list up to eight. Only list addresses at which this DroppedNeedle opens, because a page at a listed address can make changes as whoever is signed in.

That save is refused like any other from the address that is not accepted yet. Open DroppedNeedle directly at its own address and port (`http://your-server:8688`) and save it there. If you can only reach it through the proxy, set it on the host with the server stopped:

```bash
docker compose stop droppedneedle
docker compose run --rm droppedneedle droppedneedle-tool public-address https://music.example.com
docker compose up -d droppedneedle
```

Several addresses in one command replace the saved list, `--show` prints it and `--clear` empties it.

## Addresses given to player apps

Some answers tell a Jellyfin app where the server is, such as `LocalAddress` and the address to stream a track from, and Subsonic answers carry links to album and artist pictures (`artistImageUrl` and the image links of `getArtistInfo2` and `getAlbumInfo2`). That address is the one the app used: the host and port from `Host`, or from `X-Forwarded-Host` and `X-Forwarded-Proto` when the proxy is listed in `TRUSTED_PROXY_IPS`, followed by `BASE_PATH`. A proxy that ends HTTPS without sending `X-Forwarded-Proto` would make it a plain `http` address. When exactly one `https` address on that host name is listed under Public address, the app is told that one instead. The list is used for nothing else here, so an app on your network that connects by IP address keeps being told that IP address.

## HSTS

HSTS is off by default and set under Settings > Security: a max-age in seconds (0 is off, 63072000, two years, is the most it accepts), plus optional "include subdomains" and "preload", which needs "include subdomains". If your reverse proxy already adds `Strict-Transport-Security`, leave this at 0, or browsers get two headers and use the first. While it is on, every response to a request that arrived over HTTPS carries `Strict-Transport-Security` with those values, and a change applies from the next request. DroppedNeedle does not terminate TLS itself, so "arrived over HTTPS" means a proxy listed in `TRUSTED_PROXY_IPS` sent `X-Forwarded-Proto: https`. Plain-HTTP requests never get the header, and neither does a request whose `X-Forwarded-Proto` comes from an address that is not listed. A browser that has seen the header refuses plain HTTP to that hostname, on every port, until the max-age runs out, so only turn it on when nobody reaches this hostname over `http://`, and only tick "include subdomains" when everything under the domain has HTTPS.

Setting the max-age back to 0 stops the header but does not make browsers forget: each one keeps refusing plain HTTP until the max-age it last saw runs out. To end that early, set the max-age to 1, open DroppedNeedle over HTTPS once in every browser that used it, then set it to 0. Failing that, clear the site's HSTS entry in the browser. A domain that was submitted to the browsers' preload list has to be removed through hstspreload.org as well, which takes months.

## Content-security policy

The web UI is served with a content-security policy: a header that tells the browser what the page may load and run. Scripts, styles, fonts, audio, frames and connections come only from DroppedNeedle itself, plus YouTube's player and the preview clips from Deezer and iTunes, and pictures may come from any web address, so artist art and account pictures from Plex, Jellyfin or single sign-on keep showing.

Settings > Security > Content-security policy has two settings. "Policy" is `Enforce` (the default: the browser refuses what the policy does not allow), `Report only` (the browser lists in its console what the policy would refuse and refuses nothing, except for frames: who may show DroppedNeedle in a frame, and what its own frames may open) or `Off` (no policy is sent). A change applies from the next page load.

"Allowed to frame DroppedNeedle" is for dashboards such as Homarr or Organizr that show DroppedNeedle in a frame. Nobody may by default. List up to eight addresses, such as `https://home.example.com`, written like a public address: scheme, host and port, no path. Unless the policy is `Off`, nothing that is not on the list can show DroppedNeedle in a frame. A dashboard on the same site (another subdomain of the same domain, or the same host on another port) works; one on another site shows the sign-in page, because the browser does not send the sign-in cookie there.

Every other response (the API, covers, streams, files) carries a fixed policy under which nothing runs, unless the policy is `Off`. The Subsonic and Jellyfin APIs are not touched, except that the Subsonic picture address (`/subsonic/img`) always sends its own policy, under which nothing runs, whatever this setting says. If your reverse proxy adds a `Content-Security-Policy` of its own, the browser applies both, so it can only make the rules stricter, never looser.

A proxy that rewrites the page or injects a script into it (an analytics snippet, a script optimiser such as Cloudflare's Rocket Loader) leaves a blank page, because the browser refuses the changed script. To tell, open the browser's developer console: it names the content-security policy for each script it refused. The best fix is to turn the rewriting off for DroppedNeedle's address. Until then, set the policy to `Report only`. The Security tab cannot be reached from a blank page, so change it on the host with the server stopped:

```bash
docker compose stop droppedneedle
docker compose run --rm droppedneedle droppedneedle-tool content-security-policy report-only
docker compose up -d droppedneedle
```

It takes `enforce`, `report-only` or `off`.
