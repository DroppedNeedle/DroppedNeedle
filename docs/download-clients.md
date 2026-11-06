# Download clients

DroppedNeedle drives a download client you run yourself: slskd, SABnzbd with Newznab indexers or Prowlarr, or a plugin download client. It runs none of them for you. This page covers setting them up. What happens to a request after that is in [Requests and downloads](downloads.md).

## Contents

- [Download sources](#download-sources)
- [slskd](#slskd)
- [Usenet](#usenet)
- [Drop imports](#drop-imports)

## Download sources

A download source is slskd or SABnzbd once it is turned on with a URL and an API key, or an enabled plugin download client (see [PLUGINS.md](../PLUGINS.md)). It counts while it is down too: DroppedNeedle keeps retrying it, and the Download Client page shows it as not reachable. Requests, manual search, the health check and Discover's "set up a download client" prompt all go by this one answer.

Request buttons show whether or not a download source is turned on. A request made while none is on is accepted and held. The Requests and Downloads pages show it as "Waiting: no download source configured", and it starts by itself as soon as an admin turns a source on or enables a plugin download client. From then on its card shows what the download is doing: searching, awaiting review or downloading.

The Requests page also lists the downloads that auto-download starts for artists you follow, marked "Followed artist". They count toward your quota like any other request. When your request quota is full, the request still appears and waits as "Waiting: request quota full" ("Waiting: storage full" when your storage budget or the library's size cap is reached), then starts by itself once there is room.

slskd and Usenet can run side by side; the source priority control under Settings > Download Client picks who goes first.

## slskd

DroppedNeedle talks to your own running slskd over its local HTTP API (`X-API-Key`). You bring slskd; DroppedNeedle drives it.

- Use slskd 0.25.0 or newer (0.25.1 is the verified pin: `slskd/slskd:0.25.1`), with a Soulseek account inside it.
- Give DroppedNeedle the URL plus API key under Settings > Download Client, then Test and Save. The key is stored encrypted and never logged.

> [!IMPORTANT]
> Soulseek bans clients that share nothing, so give slskd at least one shared folder or searches and downloads fail. Everything in a shared folder goes out to the network. Choose it like you mean it.

The downloads path is where most installs go wrong. Three rules:

1. Expose slskd's completed-downloads directory to DroppedNeedle read-write under one shared parent mount (e.g. library at `/data/music`, completions at `/data/slskd/complete`).
2. Point `SLSKD_DOWNLOADS_PATH` at that exact directory as mounted, not its parent or a folder below it. DroppedNeedle takes the path you set as given and checks only what lies below it for links; to read a folder inside it, use the Downloads subfolder setting.
3. Skip nested binds under `/data`. Each one is a new mount boundary, which drops imports to the slower copy fallback.

Keep slskd's incomplete directory on the same mount as its downloads directory: slskd can report a transfer Completed before a cross-mount move between the two finishes, so a split mount risks verifying half-copied files.

The full annotated compose, with an optional slskd sidecar, is [docker-compose.example.yml](../docker-compose.example.yml).

<details>
<summary>Minimal slskd.yml essentials</summary>

```yaml
soulseek:
  username: your-soulseek-username
  password: your-soulseek-password

shares:
  directories:
    - /data/share   # required: share something or the network bans you

directories:
  downloads: /downloads   # the sidecar path; bind its host dir into DroppedNeedle too

web:
  authentication:
    api_keys:
      droppedneedle:
        key: choose-a-long-random-key
```

</details>

## Usenet

The second source is Usenet through SABnzbd. For searching, either add Newznab-compatible indexers one by one (NZBGeek, NZBPlanet, NZB.su, Slug, and others) or point at a Prowlarr that already has them. The engine searches your chosen side, enqueues NZBs in your SABnzbd, and imports finished files through the same scoring, verification, and quarantine pipeline as slskd.

1. Expose SABnzbd's completed-downloads directory read-write, ideally under the same shared parent mount as the library so the [mount rules](#slskd) hold. In SABnzbd, point its Downloads folder setting at the matching path (e.g. `/data/sabnzbd/complete`).
2. Under Settings > Download Client, enable Usenet, enter your SABnzbd URL and API key, and set Downloads mount to that directory as mounted, not a folder below it (only what lies below the path you set is checked for links).
3. Under Settings > Indexers / Prowlarr, pick one search backend: add each indexer's URL plus API key, or enter your Prowlarr URL plus API key. Then Test and Save.

Indexers added one by one are asked at most about once a second each. Many cap API calls and NZB grabs per day and report how much is used; DroppedNeedle keeps to those caps, skipping an indexer that has used up its day until its allowance comes back, and waits out any slow-down an indexer asks for. More in [Indexer limits](downloads.md#indexer-limits).

## Drop imports

Bought music somewhere else? Drop it in. Admins and trusted users can upload up to 25 files at once on the Downloads page: audio files, or `.zip` archives straight from the store. Each album is identified, tagged and filed, or held for a manual match. The details are under [Drop imports](downloads.md#drop-imports). For a large collection, an admin can set an [import folder](downloads.md#import-folder) that DroppedNeedle imports from and empties by itself; it needs no download client.
