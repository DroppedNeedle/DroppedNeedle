# MusicBrainz sources and self-hosted mirrors for DroppedNeedle

DroppedNeedle offers four explicit MusicBrainz source choices:

- **BrainzMash**, a read-only public pool and the Recommended choice for fresh and reset
  configuration. It is active for that configuration; administrators may additionally stage a
  consent-bound proposal for disclosure, verification, and activation.
- **Official**, the public `musicbrainz.org` service with DroppedNeedle's conservative local
  wire policy.
- **Self-hosted mirror**, a full MusicBrainz replica that you operate.
- **Community / external server**, a server operated by somebody else.

Selecting a card does not silently change the source serving runtime traffic. During normal
operation, save a source or stage/activate BrainzMash to switch it, and failures stay visible. This
guide covers the source boundary, BrainzMash consent and contribution guidance, and the full
self-hosted mirror setup.

## Upgrade migration

On upgrade, DroppedNeedle normalizes persisted MusicBrainz settings before serving requests. Missing
settings, legacy Official settings, malformed settings, and explicit Official settings migrate to
built-in BrainzMash. A valid Self-hosted mirror or Community / external setting stays selected.

This startup normalization is not a runtime fallback. Because it can make BrainzMash active, review
**Settings > MusicBrainz**, the active source, and its disclosure after upgrading. An administrator
can switch to Official, a Self-hosted mirror, or Community / external by explicitly saving it, and a
deliberate Official save persists across restarts.

## Which claim comes from where?

The labels below separate public documentation, operator statements, DroppedNeedle owner
choices, and observations made against a live surface:

- **Pinned public documentation:** the BrainzMash project README and integration guides at
  commit `c25bd6e8816592af33cac65c42dff6a35b9e0566` (2026-08-29) describe the public pool,
  its read-only `ws/2` boundary, and contributor setup.
- **Operator statement:** `https://api.brainzmash.cc/ws/2` is the public BrainzMash frontdoor
  approved for DroppedNeedle consumer reads. Consumer requests are expected to be
  credentialless. The `X-BrainzMash-Key` shown in the Nginx guide is for private contributor
  backends, not for DroppedNeedle.
- **DroppedNeedle owner decision:** BrainzMash may be embedded, distributed, labeled
  Recommended, and preselected for fresh and reset configuration. DroppedNeedle independently
  uses a local 10 requests/second, capacity-1 wire policy for BrainzMash and a 1 request/second,
  capacity-1 wire policy for Official. The app does not promise provider availability and never
  falls back silently.
- **Live observation:** a dashboard retrieval on 2026-08-24 showed recent search terms and
  partial network or location information. This is an observed exposure, not a promise that the
  dashboard always displays the same fields.

### Public links

These links are pinned where the upstream project provides a commit-specific document:

- [BrainzMash README at the pinned commit](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/README.MD)
- [Pinned DroppedNeedle integration guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-droppedneedle-support.md)
- [Pinned Nginx and frontdoor guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-brainzmash-nginx.md)
- [BrainzMash public dashboard](https://dash.brainzmash.cc/)

The dashboard is a live operational surface, not a versioned API contract. The pinned guides do
not prove every lookup, browse, include, offset, redirect, or error shape used by an application.
Those shapes still need bounded compatibility checks before DroppedNeedle relies on them.

## Using BrainzMash

### Public endpoint and read-only boundary

The built-in endpoint is:

```
https://api.brainzmash.cc/ws/2
```

The pinned DroppedNeedle guide documents read-only `GET` requests under these six route
families:

```
/ws/2/artist
/ws/2/release-group
/ws/2/release
/ws/2/recording
/ws/2/isrc
/ws/2/url
```

Each family may have one optional path segment and an optional trailing slash. Other `/ws/2`
routes are outside the documented boundary and must fail explicitly. The public material does
not establish that every query parameter, search form, browse form, include profile, pagination
offset, redirect, or error body is compatible. DroppedNeedle must verify the exact request shapes
it uses before treating them as supported.

BrainzMash is read-only for this integration. DroppedNeedle does not depend on provider cache
behavior, quorum or fingerprint internals, replication checks, freshness thresholds, or a
provider-side availability guarantee. Public material describes some of those implementation
details, but the deployed frontdoor's version and rules can differ.

### Local policy and provider limits

No published BrainzMash numeric quota, burst allowance, concurrency allowance, `Retry-After`
contract, or fair-use limit is treated as a client contract. Dashboard request or cache numbers
are observations, not an allowance or SLA.

DroppedNeedle therefore applies its own policy before every BrainzMash request and retry:

- 10 requests per second;
- token capacity 1, so local queue concurrency cannot create a wire burst;
- the Official source uses 1 request per second and the same capacity-1 wire policy; and
- explicit typed failures when the provider is unavailable or returns an unsupported result.

This local policy is not a BrainzMash quota. Raising it requires a separate written owner
decision and a new implementation review.

### Privacy disclosure

> BrainzMash receives MusicBrainz query terms and normal connection metadata. A live dashboard
> observation on 2026-08-24 showed recent search terms and partial network or location
> information. Retention duration, redaction guarantees, and exact client-IP handling are
> unknown.

This disclosure remains visible in DroppedNeedle. It does not block ordinary BrainzMash requests
when BrainzMash is the active built-in source. Consent is required for the consent-bound Test
Connection and activation flow. Do not put query terms, credentials, client identifiers, private
contributor endpoints, or keys in public issues, documentation, screenshots, telemetry, or
fixtures.

### Exact stage, consent, verification, and activation flow

BrainzMash keeps a pending proposal separate from the binding currently serving runtime
traffic. Staging may select the built-in source immediately; consent and verification then
prepare the pending identity for activation.

```json
{
  "access_revision": "opaque local value",
  "source_id": "opaque local value",
  "generation": 1,
  "disclosure_version": "current disclosure version"
}
```

The values are generated and stored locally. They are not hostnames, URL fingerprints, provider
credentials, or values to copy into public documentation.

1. **Stage the built-in endpoint.** An administrator sends `POST
/api/v1/settings/musicbrainz/brainzmash/stage` with no provider payload. The server records a
fresh pending endpoint, opaque access revision, source ID, and generation, then applies the
built-in BrainzMash source. Staging performs local work only and sends zero BrainzMash requests.
If a different source was active, the explicit stage operation switches runtime traffic to
BrainzMash; if BrainzMash was already active, the current binding continues serving.
2. **Accept the disclosure.** An administrator sends `POST
/api/v1/settings/musicbrainz/brainzmash/consent` with the exact current `access_revision`,
`source_id`, `generation`, and `disclosure_version`. This is a local zero-wire action. It
persists consent for that exact pending identity and enables Test Connection for that proposal.
3. **Test Connection.** DroppedNeedle sends `POST
/api/v1/settings/musicbrainz/verify` with the same exact binding. The dedicated BrainzMash
client uses the local 10 requests/second, capacity-1 policy. A successful test records
verification for the same endpoint and binding. A failed, stale, or mismatched test cannot
activate the proposal.
4. **Activate.** An administrator sends `POST
/api/v1/settings/musicbrainz/activate` with the same exact binding. Only a matching consent
and successful verification can atomically promote the pending proposal to the active
BrainzMash binding.

Changing the endpoint, access revision, source ID, generation, proposed source, or disclosure
version clears consent and verification. Reset invalidates the old pending identity, stages the
built-in endpoint with a fresh opaque identity, preselects BrainzMash, and sends no provider
request. Staging can replace a different active source; consent and testing do not change the
active source.

### Failure, disable, rollback, and source switching

A BrainzMash outage, 503 response, unsupported route, stale binding, failed activation, or later
runtime error remains an explicit BrainzMash error. DroppedNeedle does not call Official, a
mirror, or a Community server as a hidden fallback, and it does not guess a credential or
authentication header.

To switch away from an active BrainzMash source, an administrator selects Official, Self-hosted
mirror, or Community / external and explicitly saves that source. A connection test is
available before saving when the endpoint needs checking; the save commits the selected source
and its source-specific limits and disclosure. Selecting a card alone does not silently switch
runtime traffic. The same explicit save rule applies when moving between the other source
classes.

If BrainzMash must be withdrawn, rollback disables BrainzMash verification and runtime traffic,
removes its Recommended and fresh/reset presentation in the affected release, and requires the
administrator to choose another source. The app shows the failure and preserves the source
choice; it never redirects BrainzMash traffic to Official automatically. Existing active source
state, pending identity fencing, consent bindings, cache-generation isolation, and provenance
must remain intact during rollback.

## Contributing a mirror to the BrainzMash pool

This section describes contribution guidance, not a new in-app setup flow. Start with the pinned
public documents rather than copying their commands into this guide:

- [Pinned README](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/README.MD)
- [Pinned DroppedNeedle support guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-droppedneedle-support.md)
- [Pinned Nginx guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-brainzmash-nginx.md)

The pinned public material documents the supported read-only route families and the contributor
architecture. The following prerequisites are advisory operator guidance, not DroppedNeedle
requirements:

- a Debian-based host running Docker Compose;
- at least 8 GB of RAM and at least 100 GB of SSD or NVMe storage;
- several hours for initial download, indexing, and replication;
- a 40-character MusicBrainz replication token;
- artist, release-group, release, and recording indexes;
- a weekly search-index rebuild, with hourly replication optional; and
- a private backend URL and key handed to the pool operator, followed by an external response
  check.

Before handoff:

1. Follow the pinned project and Nginx guides for the exact image, volume, network, TLS, and
   frontdoor configuration. Do not duplicate their commands here.
2. Keep the replication token, private backend URL, private backend key, and any tunnel or
   firewall credentials out of public source, issues, screenshots, logs, and documentation.
3. Put the frontdoor behind secure TLS or a private tunnel and a deliberate firewall policy.
   Do not expose the contributor stack directly to an untrusted network.
4. Give the private endpoint and key to the operator through the operator's private handoff
   process. Rotate them when access changes and request removal when the backend should leave the
   pool.
5. Ask the operator to perform the external response check after setup. A successful check is
   evidence about that deployment, not a DroppedNeedle availability guarantee.

The `X-BrainzMash-Key` protects private contributor backends behind the frontdoor. It is not a
credential for DroppedNeedle consumer requests. DroppedNeedle sends no such header and does not
invent one if a deployment behaves differently from the operator statement. An unexpected
authentication requirement blocks activation until the actual contract is reviewed.

## Self-hosting a MusicBrainz mirror

A self-hosted mirror gives DroppedNeedle a full local copy of the MusicBrainz PostgreSQL data and
the bundled `musicbrainz-server` application. It trades dependence on a shared public service for
responsibility for your own replica. The official deployment authority is
[metabrainz/musicbrainz-docker](https://github.com/metabrainz/musicbrainz-docker).

You do not need a Lidarr metadata layer for DroppedNeedle. Community guides sometimes deploy one
alongside `musicbrainz-docker`, but DroppedNeedle speaks plain `ws/2`, which the stock stack
serves.

### 1. What you are signing up for

| Responsibility    | Reality                                                                       |
| ----------------- | ----------------------------------------------------------------------------- |
| Disk              | Plan for hundreds of GB; see the sizing section below.                        |
| Replication       | You apply upstream packets on a schedule, so freshness follows that schedule. |
| Search freshness  | Search indexes are not part of replication; you rebuild them yourself.        |
| Schema migrations | Upstream schema changes require migration on your schedule.                   |
| Security          | The stack is not hardened for public exposure by default.                     |

### 2. Pin the release

The existing deployment pin is:

```
metabrainz/musicbrainz-docker  v-2026-05-13.0-mbdb31-pg18
```

The tag was published on 2026-05-13 and corresponds to MusicBrainz Server schema sequence 31
on PostgreSQL 18. Verify the current release and migration steps against the upstream repository
before deploying or upgrading.

Follow the upstream repository's deployment documentation for `admin/configure`, `createdb.sh`,
and `replication.sh`. The upstream README is the authority on flags, images, and volume layout.
Do not treat a BrainzMash contributor guide as a replacement for official MusicBrainz deployment,
licensing, or replication guidance.

#### Upgrading past the pin

Before changing pins, read the upstream release notes for schema-sequence bumps and apply the
migration steps between the current pin and the target. Community migration notes can help locate
issues, but verify them against upstream before use.

### 3. Replication token and cadence

Replication packets are how your mirror catches up with `musicbrainz.org`:

1. Sign up for the free non-commercial Live Data Feed token at
   <https://metabrainz.org/signup/noncommercial> and put it in your DBDefs or Compose
   configuration.
2. Load the initial database with `createdb.sh -fetch`. Expect an hour or more.
3. Run an initial `replication.sh` catch-up, then schedule `replication.sh` with cron.

Upstream publishes packets hourly, which is the finest documented granularity. The
`musicbrainz-docker` default crontab applies them daily at 03:00 UTC. Running hourly, for
example at :05 past the hour, reduces typical lag, but your mirror remains only as fresh as its
replication schedule.

### 4. Search indexing

Search results come from Solr indexes that replication does not carry. The standard path is a
weekly reindex:

```sh
sir reindex --entity-type artist --entity-type release
```

A full manual reindex measured about 4.5 hours on a 16-thread, 16 GB machine. MetaBrainz also
publishes prebuilt Solr index archives of roughly 60 GB, which can shorten initial setup. You
still need a regular reindex schedule afterward.

Continuous indexing exists upstream but is marked experimental. Prefer the scheduled reindex
until upstream changes that status.

If your search indexes go stale, DroppedNeedle can still reach the mirror, but text matching and
ranking can degrade. See [What a mirror changes for DroppedNeedle's queries](#what-a-mirror-changes-for-droppedneedles-queries).

### 5. Sizing honesty

Two different sizing ranges circulate. Consider both before buying hardware:

| Source                                                  | CPU                | RAM           | Disk            |
| ------------------------------------------------------- | ------------------ | ------------- | --------------- |
| Contributor guide minimum                               | Moderately capable | At least 8 GB | At least 100 GB |
| `musicbrainz-docker` recommendation with indexed search | 16 threads         | 16 GB         | 350 GB          |
| `musicbrainz-docker` floor without indexed search       | 2 threads          | 4 GB          | 100 GB          |

The contributor minimums assume strict container memory limits and fewer indexes. Indexed search
on a small Solr heap is the first component likely to fail under memory pressure.

One operator loaded a full dump of 224 tables and about 357 million rows in roughly 35 minutes on
an 8-core, 16 GB VM. A primary-key build then failed on a 256 GB disk with 171 GB free. Leave
headroom beyond the raw dump size, especially if the host also runs DroppedNeedle.

### 6. Exposure warnings

- Bind the stack to localhost or your LAN. Do not port-forward it. The upstream README warns that
  the services are not configured to be safe for public exposure.
- The bundled standalone Solr has carried CVE-2025-24814. Keep it off untrusted networks and
  track upstream for fixed images.
- `musicbrainz-docker` does not provide the live site's edge rate-limit policy. A mirror can be
  hammered unless you put your own limits in front of it.
- Reads over `ws/2` do not require authentication on a mirror, so anyone who can reach the port
  can query it.

When a mirror is behind a public frontdoor, configure TLS, authentication, firewall rules, and
request limits deliberately. DroppedNeedle's client-side limits protect its own traffic; they do
not secure a mirror from other clients.

### 7. Licensing

MusicBrainz data is split across licenses, and replication packets carry their own terms:

- **Core data:** CC0, including the main `mbdump.tar.bz2` dump.
- **Supplementary data:** CC BY-NC-SA 3.0, including annotations, tags and genres, ratings, edit
  history, statistics, and the derived dump.
- **Live Data Feed replication packets:** CC BY-NC-SA 3.0.

For personal or non-commercial use, obtain the free token and follow the applicable terms.
Commercial use is handled through [MetaBrainz supporter accounts](https://metabrainz.org/).
Treat the licensing terms and any supporter obligations as upstream matters, not BrainzMash
policy.

### 8. Point DroppedNeedle at your mirror

In DroppedNeedle, open **Settings -> MusicBrainz** and choose **Self-hosted mirror**. Set:

- **API URL:** `http://<mirror-host>:5000/ws/2` if using the Compose stack's published port. Use
  the URL and port chosen for your deployment.
- **Rate limit:** 0.1 to 500 requests/second, or `0` for the local Unlimited sentinel on
  non-official sources.
- **Concurrent searches:** 1 to 64, as local queue capacity.

On hardware you operate, raised limits or the Unlimited sentinel can be appropriate. They do not
create a provider guarantee. Be reasonable with servers you do not own. In particular, do not set
Unlimited against a Community server merely because the UI permits that value.

Press **Test Connection** before saving. The test checks the configured server. A successful test
is not evidence that every lookup, browse, search, include, redirect, or error shape matches the
current DroppedNeedle request set.

To move between Official, a mirror, and Community / external, test and save the selected source
explicitly. DroppedNeedle does not switch to another source when this server is unavailable.

#### Checking your data vintage

To see how far behind `musicbrainz.org` your mirror is, ask its database directly:

```sh
docker exec <mb-container> psql -U musicbrainz -d musicbrainz \
  -c "SELECT max(last_replication_date) FROM replication_control;"
```

Compare the timestamp with the current date. Hours-old data can be expected for an hourly
replication schedule; days-old data means you should inspect the replication job.

### 9. Community and external endpoints

Community / external servers are an explicit user-owned choice, disclosed rather than forbidden.
The operator controls the frontdoor, data freshness, logging, and exposure posture. Review the
operator before trusting the data or sending identity-related queries.

Community warnings:

- Volunteer-run copies can catch honest mistakes but do not protect against deliberately bad data.
- One operator controls the frontdoor, so quorum among backends cannot remove that trust boundary.
- Some public dashboards may display other people's search terms or connection information.
- Unlimited is intended for hardware you run. Be polite with servers you do not own.
- Identity decisions remain enabled when you choose this source; DroppedNeedle does not silently
  downgrade them.

The settings UI requires an explicit Community acknowledgment before saving this source. That
acknowledgment is not a provider guarantee and does not transfer responsibility for the external
operator's practices to DroppedNeedle.

## What a mirror changes for DroppedNeedle's queries

A mirror can answer the same broad classes of `ws/2` request, but replication and indexing affect
them differently:

| Class                                    | Mirror behavior                                                                  |
| ---------------------------------------- | -------------------------------------------------------------------------------- |
| **Lookup** (MBID and detail fetches)     | Results depend on whether replication has applied the relevant edits.            |
| **Browse** (paged entity listings)       | Results depend on replication freshness and the requested entity shape.          |
| **Search** (text queries ranked by Solr) | Ranking can drift because search indexes are not replicated and must be rebuilt. |

Search quality usually changes before the service returns an obvious error when reindexing is
neglected. If matching quality falls after a source switch, inspect replication and the Solr
reindex schedule before filing a DroppedNeedle bug. Exact compatibility for the request shapes used
by the current application remains an engineering check, not a blanket promise from this guide.

## Sources and claim boundaries

- [MusicBrainz API rate limiting](https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting):
  the current default average is one request/second per source IP unless separately agreed;
  rate rules may change; meaningful User-Agent information is required. This page does not
  publish a DroppedNeedle per-app burst or token-capacity contract.
- [MusicBrainz Server installation guide](https://github.com/metabrainz/musicbrainz-server/blob/b7431a191ae64ac10fa7445f9ccc15b3b80bb5de/INSTALL.md):
  use upstream instructions for server installation and migration.
- [MusicBrainz Docker](https://github.com/metabrainz/musicbrainz-docker): use upstream images,
  configuration, and release notes for mirror deployment.
- [BrainzMash README at commit `c25bd6e`](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/README.MD),
  the [pinned DroppedNeedle guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-droppedneedle-support.md),
  and the [pinned Nginx guide](https://github.com/statichum/brainzmash-hearring-aid/blob/c25bd6e8816592af33cac65c42dff6a35b9e0566/docs/add-brainzmash-nginx.md):
  public project and contributor documentation used for the BrainzMash boundary above.

Public documentation describes what its authors publish. Operator statements describe the
specific frontdoor and handoff expectations. DroppedNeedle owner decisions define local policy.
The dashboard statement is a dated live observation. None of those categories supplies an SLA,
complete privacy guarantee, or permission for hidden fallback.
