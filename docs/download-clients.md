# Download clients

DroppedNeedle drives a download client you run yourself: slskd, SABnzbd or NZBGet with Newznab indexers or Prowlarr, Deemix for Deezer, or a plugin download client. It runs none of them for you. This page covers setting them up. What happens to a request after that is in [Requests and downloads](downloads.md).

## Contents

- [Download sources](#download-sources)
- [slskd](#slskd)
- [Usenet](#usenet)
- [Deemix](#deemix)
- [Drop imports](#drop-imports)

## Download sources

A download source is slskd, SABnzbd or NZBGet once it is turned on with a URL and its sign-in, Deemix once it is turned on with its URL, or an enabled plugin download client (see [PLUGINS.md](../PLUGINS.md)). It counts while it is down too: DroppedNeedle keeps retrying it, and the Download Client page shows it as not reachable. Requests, manual search, the health check and Discover's "set up a download client" prompt all go by this one answer.

Request buttons show whether or not a download source is turned on. A request made while none is on is accepted and held. The Requests page shows it as "Waiting: no download source configured" (the Downloads page says "Download client not configured"), and it starts by itself as soon as an admin turns a source on or enables a plugin download client. From then on its card shows what the download is doing: searching, awaiting review or downloading.

The Requests page also lists the downloads that auto-download starts for artists you follow, marked "Followed artist". They count toward your quota like any other request. When your request quota is full, such a download still appears and waits as "Waiting: request quota full" ("Waiting: storage full" when your storage budget or the library's size cap is reached), then starts by itself once there is room. A request you make yourself while either is full is refused with the reason, such as "Request limit reached (10 per 7 days - you've used 10)".

slskd, Usenet and Deezer can run side by side; the source priority control under Settings > Download Client picks who goes first. Deezer comes last until you move it.

## slskd

DroppedNeedle talks to your own running slskd over its local HTTP API (`X-API-Key`). You bring slskd; DroppedNeedle drives it.

- DroppedNeedle is checked against slskd 0.25.1 (`slskd/slskd:0.25.1`). Set it up with a Soulseek account inside it.
- Give DroppedNeedle the URL plus API key under Settings > Download Client, then **Test connection** and Save. The key is stored encrypted and never logged. A redirect is followed only on the same host, never from https to http, so the key never goes anywhere else: use the address slskd answers on.

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

The second source is Usenet through SABnzbd or NZBGet. For searching, either add Newznab-compatible indexers one by one (NZBGeek, NZBPlanet, NZB.su, Slug, and others) or point at a Prowlarr that already has them. The engine searches your chosen side, enqueues NZBs in your Usenet client, and imports finished files through the same scoring, verification, and quarantine pipeline as slskd.

1. Expose your client's completed-downloads directory read-write, ideally under the same shared parent mount as the library so the [mount rules](#slskd) hold. In SABnzbd, point its Downloads folder setting at the matching path (e.g. `/data/sabnzbd/complete`); in NZBGet, its `DestDir` (e.g. `/data/nzbget/completed`).
2. Under Settings > Download Client, turn on SABnzbd or NZBGet and set Downloads mount to that directory as mounted, not a folder below it (only what lies below the path you set is checked for links). SABnzbd takes its URL and full API key (Config > General), not the NZB key. A redirect from SABnzbd is followed only on the same host and port, never from https to http, so its key never goes anywhere else: use the address SABnzbd answers on. NZBGet takes its URL and the control username and password from Settings > Security, the same sign-in as its web UI.
3. Under Settings > Indexers / Prowlarr, pick one search backend: add each indexer's URL plus API key, or enter your Prowlarr URL plus API key. Then Test and Save. A redirect from Prowlarr's API is followed only on the same host, never from https to http, so its key never goes anywhere else: use the address Prowlarr answers on. An NZB link may redirect at most five times, each time to a plain web address with no user name or password in it; a release whose link goes anywhere else is not downloaded, and the log says so.

New Usenet downloads go to one client. With both turned on that is SABnzbd, and NZBGet's card says so; turn SABnzbd off to send them to NZBGet. A download already sent stays with the client that took it and is finished from there, even after you switch, as long as that client is still set up. While that client doesn't answer, its downloads wait and the other client's carry on. Clear a client's URL and its downloads move on: each album is fetched again through the client in use. Whatever they already downloaded stays in that client's own folder, which DroppedNeedle no longer cleans up. With no Usenet client turned on, Usenet downloads wait until one is.

Indexers added one by one are asked at most about once a second each. Many cap API calls and NZB grabs per day and report how much is used; DroppedNeedle keeps to those caps, skipping an indexer that has used up its day until its allowance comes back, and waits out any slow-down an indexer asks for. More in [Indexer limits](downloads.md#indexer-limits).

## Deemix

Deemix is a program that downloads albums and songs from Deezer. You run its container yourself, as you would slskd. DroppedNeedle asks Deemix for an album or a song, Deemix saves the files to a folder DroppedNeedle reads, and DroppedNeedle checks and files them like any other download. This page uses the `ghcr.io/bambanah/deemix` image. DroppedNeedle is checked against deemix-webui 4.8.0 with deemix 3.14.1. After you update Deemix, press **Test connection** under Settings > Download Client.

Put Deemix on a Docker network that only it and DroppedNeedle share, with no published port. Set `PUID` and `PGID` to the same values as DroppedNeedle's (1000 by default, as in the example compose), so the files Deemix writes are ones DroppedNeedle can read and remove.

```yaml
services:
  droppedneedle:   # your existing service, plus the deemix network
    networks:
      - default
      - deemix
    volumes:
      - /path/to/media:/data:rw

  deemix:
    image: ghcr.io/bambanah/deemix:v4.8.0   # the version DroppedNeedle is checked against
    restart: unless-stopped
    environment:
      - PUID=1000
      - PGID=1000
      - DEEMIX_SINGLE_USER=true   # keep it true
    volumes:
      - ./deemix/config:/config            # its data: the queue and the Deezer sign-in
      - /path/to/media/deemix:/downloads   # the folder DroppedNeedle sees as /data/deemix
    networks:
      - deemix
    # ports:                               # only while you sign in, see below
    #   - "127.0.0.1:6595:6595"

networks:
  deemix:
```

1. Start Deemix with its `ports` lines uncommented. Open `http://localhost:6595` on the machine that runs Docker (or through an SSH tunnel to it) and sign in to Deezer there.
2. In Deemix's settings, set Overwrite files to Yes, turn off every fallback option and leave its command after downloads empty. Keep its download folder at `/downloads`.
3. Comment out the `ports` lines again and recreate the container. Its sign-in and queue stay in the `config` folder.
4. Under Settings > Download Client, turn on Deemix, set URL to `http://deemix:6595` and Downloads folder to `/data/deemix`, then **Test connection** and Save. `/data/deemix` is the same host folder as Deemix's `/downloads`, seen through DroppedNeedle's shared mount. **Test connection** shows Deemix's version, whether it reaches Deezer, the quality your Deezer account gives, and anything still to change.

> [!IMPORTANT]
> Deemix has no password. Anyone who can reach its port can read your Deezer sign-in and run commands on its machine. Keep it on the private network, and publish its port only to `127.0.0.1`, only while you sign in.

Deemix's download folder belongs to DroppedNeedle, never to your library. A download there replaces a file with the same name, your own included, and imported files are removed from it. Albums you downloaded there yourself are taken over when DroppedNeedle wants them. Keep your own music in another folder.

Keep Overwrite files on Yes and the fallback options off. Without Overwrite files on Yes, Deezer waits and the Deemix card says why. **Test connection** also warns about single-user mode being off and a command set after downloads.

DroppedNeedle keeps no Deezer sign-in of its own. When it needs one, it gives Deemix the sign-in Deemix saved, in memory only. If Deemix loses that sign-in, the Deemix card says so, and you sign in again on Deemix's page.

Signing out on Deemix's page does not stop DroppedNeedle until Deemix restarts. To stop DroppedNeedle using your account, sign out there and restart Deemix. After you upgrade your Deezer plan, restart Deemix: DroppedNeedle signs in again and reads the new plan. A downgrade is noticed by itself.

Using Deemix is your own responsibility.

## Drop imports

Bought music somewhere else? Drop it in. Admins and trusted users can upload up to 25 files at once on the Downloads page: audio files, or `.zip` archives straight from the store. Each album is identified, tagged and filed, or held for a manual match. The details are under [Drop imports](downloads.md#drop-imports). For a large collection, an admin can set an [import folder](downloads.md#import-folder) that DroppedNeedle imports from and empties by itself; it needs no download client.
