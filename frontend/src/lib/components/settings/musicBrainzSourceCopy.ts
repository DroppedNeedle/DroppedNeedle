import type { MusicBrainzSourceMode } from '$lib/queries/musicbrainz/types';

export type { MusicBrainzSourceMode } from '$lib/queries/musicbrainz/types';

export const MUSICBRAINZ_GUIDE_HREF = '/docs/musicbrainz-mirror-selfhosting.md';
export const BRAINZMASH_ENDPOINT_URL = 'https://api.brainzmash.cc/ws/2';
export const BRAINZMASH_DISCLOSURE_VERSION = '2026-08-31';
export const BRAINZMASH_SUPPORTED_ROUTE_FAMILIES = [
	'artist',
	'release-group',
	'release',
	'recording',
	'isrc',
	'url'
] as const;

/** Public sources use explicit provider-specific wire policies. */
export const OFFICIAL_RATE_MAX = 1;
export const OFFICIAL_CONCURRENT_MAX = 1;
export const BRAINZMASH_RATE_MAX = 10;
export const BRAINZMASH_CONCURRENT_MAX = 1;
export const NON_OFFICIAL_RATE_MAX = 500;
export const NON_OFFICIAL_CONCURRENT_MAX = 64;
export const UNLIMITED_RATE_SENTINEL = 0;

export interface SourceBounds {
	rateMax: number;
	concurrentMax: number;
	allowUnlimitedRate: boolean;
}

export function sourceBounds(mode: MusicBrainzSourceMode): SourceBounds {
	if (mode === 'brainzmash') {
		return {
			rateMax: BRAINZMASH_RATE_MAX,
			concurrentMax: BRAINZMASH_CONCURRENT_MAX,
			allowUnlimitedRate: false
		};
	}
	if (mode === 'official') {
		return {
			rateMax: OFFICIAL_RATE_MAX,
			concurrentMax: OFFICIAL_CONCURRENT_MAX,
			allowUnlimitedRate: false
		};
	}
	return {
		rateMax: NON_OFFICIAL_RATE_MAX,
		concurrentMax: NON_OFFICIAL_CONCURRENT_MAX,
		allowUnlimitedRate: true
	};
}

export function displayVerifyMessage(
	message: string,
	source: MusicBrainzSourceMode | boolean
): string {
	const mode = typeof source === 'boolean' ? (source ? 'official' : 'mirror') : source;
	if (mode === 'official' || !/rate-limited/i.test(message)) return message;
	return mode === 'brainzmash'
		? 'Connected, but BrainzMash returned 503 (busy). The provider response is explicit; no other source will be used automatically.'
		: 'Connected, but the server returned 503 (busy). On a mirror this usually means the search index is still building or the service is starting up - it is not a rate-limit problem, so there is nothing to lower.';
}

export interface SourceCardCopy {
	mode: MusicBrainzSourceMode;
	title: string;
	badge: string | null;
	blurb: string;
}

export const MUSICBRAINZ_SOURCE_CARDS: SourceCardCopy[] = [
	{
		mode: 'brainzmash',
		title: 'BrainzMash',
		badge: 'Recommended',
		blurb: 'A read-only public MusicBrainz pool with no consumer credential required.'
	},
	{
		mode: 'official',
		title: 'Official',
		badge: null,
		blurb: "The public musicbrainz.org API, with DroppedNeedle's conservative local 1/1 policy."
	},
	{
		mode: 'mirror',
		title: 'Self-hosted mirror',
		badge: null,
		blurb: 'Your own full copy of the MusicBrainz database, on your hardware.'
	},
	{
		mode: 'community',
		title: 'Community / external server',
		badge: null,
		blurb: "Somebody else's server answering MusicBrainz queries for you."
	}
];

export const MORE_INFO_SUMMARY = 'More info';

export const BRAINZMASH_PRIVACY_DISCLOSURE =
	'BrainzMash receives MusicBrainz query terms and normal connection metadata. Recent search terms and partial network or location information have appeared on its public dashboard. Retention, redaction, and exact client-IP handling are unspecified.';

export const BRAINZMASH_TRANSPORT_DISABLED_COPY =
	'BrainzMash is the built-in source. This optional disclosure proposal remains transport-disabled until it is reviewed.';
export const BRAINZMASH_LOCAL_POLICY_COPY =
	'DroppedNeedle local wire policy: 10 requests/second with token capacity 1. This is a local safety limit, not a provider quota or SLA.';
export const BRAINZMASH_ACTIVE_BINDING_COPY =
	'BrainzMash is the active runtime source. Optional disclosure metadata may be reviewed without interrupting traffic.';
export const BRAINZMASH_PENDING_TRANSPORT_COPY =
	'BrainzMash remains active while the optional disclosure binding is reviewed.';
export const BRAINZMASH_NO_ALTERNATE_PROBE_COPY =
	'Test Connection is unavailable while BrainzMash is active because no non-Brainz provider traffic may run before this source switch.';

export const MORE_INFO_DISCLOSURES: Record<MusicBrainzSourceMode, string[]> = {
	brainzmash: [
		'Read-only support covers artist, release-group, release, recording, ISRC, and URL MusicBrainz route families. DroppedNeedle does not depend on provider cache, quorum, freshness, or availability guarantees.',
		BRAINZMASH_PRIVACY_DISCLOSURE,
		BRAINZMASH_LOCAL_POLICY_COPY
	],
	official: [
		'MusicBrainz documents a current default average of 1 request/second per source IP unless separately agreed. Its rate rules may change, and meaningful User-Agent information is required.',
		'DroppedNeedle keeps Official traffic on a conservative local 1 request/second, capacity-1 wire policy. Local queue concurrency must not create a burst.'
	],
	mirror: [
		'Running a mirror means hosting your own full copy of the MusicBrainz database. Be honest with yourself about the footprint: roughly 8-16 GB of RAM and 100-350 GB of disk, plus a weekly search-index rebuild.',
		'Your data is exactly as fresh as your replication schedule, and search quality depends on your reindex cadence - which is precisely why raised or unlimited request limits are fine here. It is your machine; you set the rules.',
		'The setup guide walks through musicbrainz-docker, the replication token, sizing, and the one-line command to check your data vintage.'
	],
	community: [
		"What this is: somebody else's server answers MusicBrainz queries for you. Volunteer-run mirror copies catch honest mistakes - they do not protect against deliberately bad data, and one operator controls the front door. Some public dashboards even display other people's search terms.",
		'Politeness: "Unlimited" is meant for your own hardware. Be reasonable with servers you do not own.',
		'Your choice, your ownership: identity decisions made while connected to this server stay fully enabled - nothing is blocked or downgraded.'
	]
};

export const COMMUNITY_RISK_BANNER =
	'Community servers are run by volunteers: trust the operator before you trust the data. Quorum-style copies catch accidents, not coordinated bad data; one person controls the front door, and some public dashboards leak search terms.';

export const COMMUNITY_CONFIRM_LABEL =
	'I understand the risks of routing identity data through a server I do not control.';

export const COMMUNITY_CONFIRM_BUTTON_HINT = 'Acknowledge the risk notice to enable saving.';

export const MIRROR_BANNER_LINES = [
	"You are using a non-official MusicBrainz endpoint. Search results depend on that server's reindex schedule - search indexes are not replicated.",
	"Metadata freshness is bounded by the operator's replication cron.",
	'Identity verification quality depends on the operator.'
];

export const CLAMPED_WARNING =
	'Values were clamped to the conservative Official 1 request/second, capacity-1 local policy.';

export const UNLIMITED_RATE_LABEL = 'Unlimited';

export const OFFICIAL_ENDPOINT_URL = 'https://musicbrainz.org/ws/2';
