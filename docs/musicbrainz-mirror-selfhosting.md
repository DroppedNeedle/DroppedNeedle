# MusicBrainz sources and self-hosted mirrors

DroppedNeedle looks up artists, albums and tracks on MusicBrainz. Under **Settings > MusicBrainz** you pick which server answers those lookups. There are four choices:

- **BrainzMash** (Recommended): a read-only public pool of MusicBrainz copies. A new install starts on it.
- **Official**: the public `musicbrainz.org` service, at a careful pace DroppedNeedle sets.
- **Self-hosted mirror**: a full MusicBrainz copy that you run.
- **Community / external server**: a server someone else runs.

Clicking a card changes nothing until you save it (or, for BrainzMash, stage it). A saved choice stays across restarts. DroppedNeedle never moves to another source by itself: when the chosen one fails, you see its errors.

This page covers which source you start on, using BrainzMash, contributing a mirror to the BrainzMash pool, and setting up your own mirror. Where the details come from is at the end.

## Contents

- [The starting source](#the-starting-source)
- [Using BrainzMash](#using-brainzmash)
- [Contributing a mirror to the BrainzMash pool](#contributing-a-mirror-to-the-brainzmash-pool)
- [Self-hosting a MusicBrainz mirror](#self-hosting-a-musicbrainz-mirror)
- [What a mirror changes for DroppedNeedle's queries](#what-a-mirror-changes-for-droppedneedles-queries)
- [Where these details come from](#where-these-details-come-from)

## The starting source

A server with no MusicBrainz settings, a fresh install or a reset one, starts on BrainzMash. Settings carried over from v2 keep their source, Official included. Settings that cannot be read stop MusicBrainz lookups until they are saved again.

Check **Settings > MusicBrainz** after setting up or upgrading. An admin can switch to Official, a Self-hosted mirror or Community / external by saving it.

## Using BrainzMash

### What it answers

DroppedNeedle has BrainzMash built in, at:

```
https://api.brainzmash.cc/ws/2
```

It sends read-only `GET` requests, with no key or other credential, under these six route families:

```
/ws/2/artist
/ws/2/release-group
/ws/2/release
/ws/2/recording
/ws/2/isrc
/ws/2/url
```

Each family may have one optional path segment and an optional trailing slash. Other `/ws/2` routes are outside what BrainzMash documents, and a request for one fails with an error. BrainzMash's public material does not show that every query parameter, search form, browse form, include profile, pagination offset, redirect or error body works, so DroppedNeedle treats a request shape as supported only once it has been checked.

DroppedNeedle does not rely on how BrainzMash caches, its quorum or fingerprint internals, its replication checks or freshness thresholds, or any promise that it stays up. Its public material describes some of these, but the version and rules of the deployed service can differ.

### Pace

BrainzMash publishes no numeric quota, burst or concurrency allowance, `Retry-After` contract or fair-use limit, and DroppedNeedle treats none as a contract. Request and cache numbers on its dashboard are observations, not an allowance or a service level.

DroppedNeedle applies its own limits before every BrainzMash request and retry:

- 10 requests a second;
- a token capacity of 1 and one request in flight at a time, so its own queues cannot send a burst;
- after a 429, a pause for the `Retry-After` it names, up to a minute, or one that doubles from 1 second to a minute while 429s name no wait;
- the Official source gets 1 request a second with the same capacity of 1; and
- a typed error when the provider is down or answers something unsupported.

These limits are fixed in DroppedNeedle, not a setting, and they are not a BrainzMash quota.

### Privacy

> BrainzMash receives MusicBrainz query terms and normal connection metadata. A live dashboard
> observation on 2026-08-24 showed recent search terms and partial network or location
> information. Retention duration, redaction guarantees, and exact client-IP handling are
> unknown.

DroppedNeedle shows this notice on the settings page. It does not stop ordinary BrainzMash requests while BrainzMash is the active built-in source; consent is needed only for the Test Connection and activation steps below. Keep query terms, credentials, client identifiers, private contributor endpoints and keys out of public issues, documentation, screenshots, telemetry and fixtures.

### Stage, consent, test and activate

BrainzMash keeps a pending proposal apart from the source serving lookups now. Staging can switch to the built-in source straight away; consent and a test then ready the proposal for activation. A proposal is named by four values:

```json
{
  "access_revision": "opaque local value",
  "source_id": "opaque local value",
  "generation": 1,
  "disclosure_version": "current disclosure version"
}
```

The server makes these values itself and keeps them in memory. They are not hostnames, URL fingerprints or provider credentials, and they do not belong in public documentation.

1. **Stage the built-in endpoint.** An admin sends `POST /api/v3/settings/musicbrainz/brainzmash/stage` with no provider payload. The server records a fresh pending endpoint, access revision, source ID and generation, then applies the built-in BrainzMash source. Staging is local work only and sends no request to BrainzMash. If another source was active, staging switches lookups to BrainzMash; if BrainzMash was already active, it carries on serving.
2. **Accept the notice.** An admin sends `POST /api/v3/settings/musicbrainz/brainzmash/consent` with the exact current `access_revision`, `source_id`, `generation` and `disclosure_version`. This is local too and sends nothing. It records consent for that exact proposal and turns on Test Connection for it. The proposal and its consent live in memory, so a restart drops them and staging starts again.
3. **Test Connection.** DroppedNeedle sends `POST /api/v3/settings/musicbrainz/verify` with the same values. The test goes through the BrainzMash client, under the same 10 requests a second and capacity of 1 as every other BrainzMash request. A test that passes records the check for that endpoint and those values. A failed, stale or mismatched test cannot activate the proposal.
4. **Activate.** An admin sends `POST /api/v3/settings/musicbrainz/activate` with the same values. Only a matching consent and a passed test promote the pending proposal, in one step, to the active BrainzMash source.

Staging again, saving another source or restarting the server drops the pending proposal with its consent and test. A consent, test or activation that names other values is refused as stale. Reset drops the old pending values, stages the built-in endpoint with fresh ones, preselects BrainzMash and sends nothing to the provider. Staging can replace another active source; consent and testing never change the active source.

### When BrainzMash fails, and switching away

An outage, a 503, an unsupported route, stale values, a failed activation or a later error stays a BrainzMash error. DroppedNeedle does not call Official, a mirror or a Community server in its place, and it does not guess a credential or an authentication header.

To switch away from BrainzMash, an admin selects Official, Self-hosted mirror or Community / external and saves it. While BrainzMash is active, Test Connection is off for the other sources: save the new source to switch, and its failures then show as errors from that source. Between the other sources, test before saving. Saving commits the selected source with its own limits and notice. The same rule, nothing changes until you save, applies between every pair of sources.

## Contributing a mirror to the BrainzMash pool

This is guidance for running a backend in the BrainzMash pool, not something DroppedNeedle sets up for you. Start from the pinned public documents rather than copying their commands here:

- [Pinned README](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/README.MD)
- [Pinned DroppedNeedle support guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-droppedneedle-support.md)
- [Pinned Nginx guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-brainzmash-nginx.md)

They document the read-only route families and how contributor backends fit together. The operator advises the following; DroppedNeedle requires none of it:

- a Debian-based host running Docker Compose;
- at least 8 GB of RAM and at least 100 GB of SSD or NVMe storage;
- several hours for the first download, indexing and replication;
- a 40-character MusicBrainz replication token;
- artist, release-group, release and recording indexes;
- a weekly search-index rebuild, with hourly replication optional; and
- a private backend URL and key handed to the pool operator, followed by a check from outside that the backend answers.

Before handing it over:

1. Follow the pinned project and Nginx guides for the exact image, volume, network, TLS and frontdoor setup.
2. Keep the replication token, private backend URL, private backend key and any tunnel or firewall credentials out of public source, issues, screenshots, logs and documentation.
3. Put the frontdoor behind TLS or a private tunnel and a deliberate firewall policy. Do not expose the contributor stack directly to an untrusted network.
4. Give the private endpoint and key to the operator through their private handover. Rotate them when access changes, and ask for removal when the backend should leave the pool.
5. Ask the operator to check the backend from outside once it is set up. A passed check says something about that deployment, not that DroppedNeedle will always reach it.

The `X-BrainzMash-Key` protects private contributor backends behind the frontdoor. It is not a credential for DroppedNeedle's lookups: DroppedNeedle sends no such header, and does not make one up if a deployment behaves differently from what the operator stated. An unexpected demand for authentication blocks activation until someone has looked at the real contract.

## Self-hosting a MusicBrainz mirror

A self-hosted mirror gives DroppedNeedle a full local copy of the MusicBrainz PostgreSQL data and the bundled `musicbrainz-server` application. You stop depending on a shared public service and take on your own copy instead. The official deployment is [metabrainz/musicbrainz-docker](https://github.com/metabrainz/musicbrainz-docker).

You do not need a Lidarr metadata layer for DroppedNeedle. Community guides sometimes deploy one alongside `musicbrainz-docker`, but DroppedNeedle speaks plain `ws/2`, which the stock stack serves.

### 1. What you are signing up for

| Responsibility    | Reality                                                                       |
| ----------------- | ----------------------------------------------------------------------------- |
| Disk              | Plan for hundreds of GB; see the sizing section below.                        |
| Replication       | You apply upstream packets on a schedule, so freshness follows that schedule. |
| Search freshness  | Search indexes are not part of replication; you rebuild them yourself.        |
| Schema migrations | Upstream schema changes require migration on your schedule.                   |
| Security          | The stack is not hardened for public exposure by default.                     |

### 2. Pin the release

The current pin is:

```
metabrainz/musicbrainz-docker  v-2026-05-13.0-mbdb31-pg18
```

The tag was published on 2026-05-13 and matches MusicBrainz Server schema sequence 31 on PostgreSQL 18. Check the current release and migration steps against the upstream repository before deploying or upgrading.

Follow the upstream repository's documentation for `admin/configure`, `createdb.sh` and `replication.sh`. Its README is the authority on flags, images and volume layout. A BrainzMash contributor guide does not replace the official MusicBrainz deployment, licensing or replication guidance.

#### Upgrading past the pin

Before changing pins, read the upstream release notes for schema-sequence bumps and apply the migration steps between the current pin and the target. Community migration notes can help find problems, but check them against upstream before you use them.

### 3. Replication token and schedule

Replication packets are how your mirror catches up with `musicbrainz.org`:

1. Sign up for the free non-commercial Live Data Feed token at <https://metabrainz.org/signup/noncommercial> and put it in your DBDefs or Compose configuration.
2. Load the first database with `createdb.sh -fetch`. Expect an hour or more.
3. Run `replication.sh` once to catch up, then schedule it with cron.

Upstream publishes packets hourly, the finest step it documents. The `musicbrainz-docker` default crontab applies them daily at 03:00 UTC. Running hourly, for example at 5 past the hour, cuts the usual lag, but your mirror is only ever as fresh as its replication schedule.

### 4. Search indexing

Search results come from Solr indexes that replication does not carry. The usual path is a weekly reindex:

```sh
sir reindex --entity-type artist --entity-type release
```

A full manual reindex took about 4.5 hours on a 16-thread, 16 GB machine. MetaBrainz also publishes prebuilt Solr index archives of roughly 60 GB, which can shorten the first setup. You still need a regular reindex afterwards.

Continuous indexing exists upstream but is marked experimental. Prefer the scheduled reindex until upstream changes that.

If your search indexes go stale, DroppedNeedle can still reach the mirror, but text matching and ranking can get worse. See [What a mirror changes for DroppedNeedle's queries](#what-a-mirror-changes-for-droppedneedles-queries).

### 5. Sizing

Two different sizing ranges circulate. Weigh both before buying hardware:

| Source                                                  | CPU                | RAM           | Disk            |
| ------------------------------------------------------- | ------------------ | ------------- | --------------- |
| Contributor guide minimum                               | Moderately capable | At least 8 GB | At least 100 GB |
| `musicbrainz-docker` recommendation with indexed search | 16 threads         | 16 GB         | 350 GB          |
| `musicbrainz-docker` floor without indexed search       | 2 threads          | 4 GB          | 100 GB          |

The contributor minimums assume strict container memory limits and fewer indexes. Indexed search on a small Solr heap is usually the first thing to fail when memory runs short.

One operator loaded a full dump of 224 tables and about 357 million rows in roughly 35 minutes on an 8-core, 16 GB VM. A primary-key build then failed on a 256 GB disk with 171 GB free. Leave room beyond the raw dump size, especially if the host also runs DroppedNeedle.

### 6. Keep it off the internet

- Bind the stack to localhost or your LAN. Do not port-forward it. The upstream README warns that the services are not set up to be safe on the public internet.
- The bundled standalone Solr has carried CVE-2025-24814. Keep it off untrusted networks and watch upstream for fixed images.
- `musicbrainz-docker` does not come with the live site's rate limits. Anyone who can reach a mirror can flood it unless you put your own limits in front.
- Reads over `ws/2` need no authentication on a mirror, so anyone who can reach the port can query it.

When a mirror sits behind a public frontdoor, set up TLS, authentication, firewall rules and request limits on purpose. DroppedNeedle's own limits only pace its own traffic; they do not protect a mirror from other clients.

### 7. Licensing

MusicBrainz data is split across licenses, and replication packets carry their own terms:

- **Core data:** CC0, including the main `mbdump.tar.bz2` dump.
- **Supplementary data:** CC BY-NC-SA 3.0, including annotations, tags and genres, ratings, edit history, statistics, and the derived dump.
- **Live Data Feed replication packets:** CC BY-NC-SA 3.0.

For personal or non-commercial use, get the free token and follow the terms that apply. Commercial use goes through [MetaBrainz supporter accounts](https://metabrainz.org/). The licensing terms and any supporter obligations are MetaBrainz's, not BrainzMash's.

### 8. Point DroppedNeedle at your mirror

Open **Settings > MusicBrainz** and choose **Self-hosted mirror**. Set:

- **Mirror API Endpoint URL:** `http://<mirror-host>:5000/ws/2` with the Compose stack's published port, or the URL and port your deployment uses.
- **Rate limit:** 0.1 to 500 requests a second, or `0` for Unlimited, which only a mirror or a Community server accepts.

On hardware you run, a higher limit or Unlimited can make sense, though it promises nothing about the server. Be reasonable with servers you do not own: do not set Unlimited against a Community server just because the form allows it.

Press **Test Connection** before saving. While BrainzMash is the active source the test is off, so save and watch for errors. The test checks the server you entered, paced like real traffic: at the rate you entered, or at 1 request a second when the address is a public MusicBrainz host. A passed test does not prove that every lookup, browse, search, include, redirect or error shape DroppedNeedle uses works there.

To move between Official, a mirror and Community / external, test and save the source you selected. DroppedNeedle does not switch to another source when this server is down.

#### Checking how fresh your data is

To see how far behind `musicbrainz.org` your mirror is, ask its database:

```sh
docker exec <mb-container> psql -U musicbrainz -d musicbrainz \
  -c "SELECT max(last_replication_date) FROM replication_control;"
```

Compare the timestamp with today. Data a few hours old is normal on an hourly schedule; data days old means the replication job needs a look.

### 9. Community and external servers

A Community / external server is your choice to make, with the risks shown rather than the option taken away. Its operator controls the frontdoor, how fresh the data is, what is logged and how exposed it is. Look into the operator before you trust the data or send queries that could identify you.

Worth knowing:

- Volunteer-run copies can catch honest mistakes but do not protect against deliberately bad data.
- One operator controls the frontdoor, so agreement among its backends cannot remove the need to trust that operator.
- Some public dashboards may show other people's search terms or connection information.
- Unlimited is meant for hardware you run. Be polite with servers you do not own.
- Identity decisions stay on when you choose this source; DroppedNeedle does not quietly turn them down.

The settings page asks you to acknowledge these points before saving this source. The acknowledgment is not a promise from the provider, and it does not make DroppedNeedle responsible for how the other operator runs things.

## What a mirror changes for DroppedNeedle's queries

A mirror answers the same broad kinds of `ws/2` request, but replication and indexing affect them differently:

| Kind                                     | On a mirror                                                                      |
| ---------------------------------------- | -------------------------------------------------------------------------------- |
| **Lookup** (MBID and detail fetches)     | Results depend on whether replication has applied the relevant edits.            |
| **Browse** (paged entity listings)       | Results depend on replication freshness and the requested entity shape.          |
| **Search** (text queries ranked by Solr) | Ranking can drift because search indexes are not replicated and must be rebuilt. |

When reindexing is neglected, search quality usually drops before the service shows any obvious error. If matching gets worse after a source switch, check replication and the Solr reindex schedule before filing a DroppedNeedle bug. Whether every request shape the app uses works on a given mirror is still something to check, not something this page can promise.

## Where these details come from

The details on this page come from four places: public documentation, statements from the BrainzMash operator, DroppedNeedle's own policy, and observations of the live service.

- **Pinned public documentation:** the BrainzMash project README and integration guides at commit `c25bd6e8816592af33cac65c42dff6a35b9e0566` (2026-08-29) describe the public pool, its read-only `ws/2` boundary and contributor setup.
- **Operator statement:** `https://api.brainzmash.cc/ws/2` is the public BrainzMash frontdoor approved for DroppedNeedle's reads. Those requests are expected to carry no credentials. The `X-BrainzMash-Key` shown in the Nginx guide is for private contributor backends, not for DroppedNeedle.
- **DroppedNeedle policy:** DroppedNeedle ships BrainzMash built in, labels it Recommended, and preselects it for a fresh or reset configuration. It paces BrainzMash at 10 requests a second and Official at 1 request a second, each with a capacity of 1. The app does not promise that any provider is available, and never falls back to another source by itself.
- **Live observation:** the dashboard, read on 2026-08-24, showed recent search terms and partial network or location information. This is something seen once, not a promise that the dashboard always shows the same fields.

Public documentation describes what its authors publish. Operator statements describe that one frontdoor and its handover expectations. DroppedNeedle's own policy sets the local limits. The dashboard note is a dated observation. None of them is a service level, a full privacy guarantee, or permission for a hidden fallback.

### Links

These links are pinned to a commit where the upstream project offers one:

- [BrainzMash README at commit `c25bd6e`](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/README.MD)
- [Pinned DroppedNeedle integration guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-droppedneedle-support.md)
- [Pinned Nginx and frontdoor guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-brainzmash-nginx.md)
- [BrainzMash public dashboard](https://dash.brainzmash.cc/): a live page, not a versioned API contract.
- [MusicBrainz API rate limiting](https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting): the current default average is one request a second per source IP unless agreed otherwise; the rules may change; a meaningful User-Agent is required. This page does not publish a per-app burst or token-capacity contract for DroppedNeedle.
- [MusicBrainz Server installation guide](https://github.com/metabrainz/musicbrainz-server/blob/b7431a191ae64ac10fa7445f9ccc15b3b80bb5de/INSTALL.md): use upstream instructions for installing and migrating the server.
- [MusicBrainz Docker](https://github.com/metabrainz/musicbrainz-docker): use upstream images, configuration and release notes for a mirror.

The pinned guides do not prove every lookup, browse, include, offset, redirect or error shape an application uses. Those shapes still need checking before DroppedNeedle relies on them.
