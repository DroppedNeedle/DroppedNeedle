/**
 * View logic for the admin Diagnostics settings card: labels, the grouping
 * of the outbound calls table and the polling gate.
 */
import type { components } from '$lib/api/v3/openapi';

type ProviderCallsView = components['schemas']['ProviderCallsView'];
type RequestPriority = components['schemas']['RequestPriority'];
type CallOutcome = components['schemas']['CallOutcome'];

/** Display names for the rate rows; an unknown row falls back to its name. */
export const PROVIDER_LABELS: Readonly<Record<string, string>> = {
	musicbrainz: 'MusicBrainz',
	brainzmash: 'BrainzMash',
	'musicbrainz-mirror': 'MusicBrainz mirror',
	listenbrainz: 'ListenBrainz',
	lastfm: 'Last.fm',
	audiodb: 'AudioDB',
	'audiodb-premium': 'AudioDB premium',
	'audiodb-media': 'AudioDB images',
	acoustid: 'AcoustID',
	coverartarchive: 'Cover Art Archive',
	'wikimedia-api': 'Wikimedia',
	'wikimedia-media': 'Wikimedia images',
	discogs: 'Discogs',
	itunes: 'iTunes',
	deezer: 'Deezer',
	lrclib: 'LRCLIB',
	youtube: 'YouTube',
	github: 'GitHub',
	ticketmaster: 'Ticketmaster',
	skiddle: 'Skiddle',
	'open-meteo': 'Open-Meteo'
};

export const PRIORITY_LABELS: Readonly<Record<RequestPriority, string>> = {
	user_initiated: 'User',
	image_fetch: 'Artwork',
	prefetch_visible: 'Prefetch',
	background_sync: 'Background',
	opportunistic: 'Low'
};

export const OUTCOME_LABELS: Readonly<Record<CallOutcome, string>> = {
	ok: 'OK',
	not_found: 'Not found',
	rate_limited: 'Rate limited',
	failed: 'Failed'
};

const PRIORITY_ORDER: readonly RequestPriority[] = [
	'user_initiated',
	'image_fetch',
	'prefetch_visible',
	'background_sync',
	'opportunistic'
];
const OUTCOME_ORDER: readonly CallOutcome[] = ['ok', 'not_found', 'rate_limited', 'failed'];

/** 'user_initiated' -> 'User initiated'; unknown wire values stay readable. */
export function humanizeWireValue(value: string): string {
	const spaced = value.replaceAll('_', ' ').trim();
	if (!spaced) return value;
	return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

export function providerLabel(provider: string): string {
	return PROVIDER_LABELS[provider] ?? humanizeWireValue(provider);
}

export function formatCount(count: number): string {
	return count.toLocaleString('en-US');
}

export interface CallRowView {
	key: string;
	priority: string;
	outcome: string;
	calls: number;
	lastMinute: number;
	bodyBytes: number;
}

export interface CallGroupView {
	source: string;
	label: string;
	totalCalls: number;
	rows: CallRowView[];
}

/** One group per provider, in label order; inside a group, rows run by
 * priority and then outcome, so the table reads the same between polls. */
export function groupCallRows(rows: readonly ProviderCallsView[]): CallGroupView[] {
	const groups = new Map<string, CallGroupView>();
	const sorted = [...rows].sort(
		(a, b) =>
			PRIORITY_ORDER.indexOf(a.priority) - PRIORITY_ORDER.indexOf(b.priority) ||
			OUTCOME_ORDER.indexOf(a.outcome) - OUTCOME_ORDER.indexOf(b.outcome)
	);
	for (const row of sorted) {
		let group = groups.get(row.source);
		if (!group) {
			group = { source: row.source, label: providerLabel(row.source), totalCalls: 0, rows: [] };
			groups.set(row.source, group);
		}
		group.totalCalls += row.calls;
		group.rows.push({
			key: `${row.priority}:${row.outcome}`,
			priority: PRIORITY_LABELS[row.priority],
			outcome: OUTCOME_LABELS[row.outcome],
			calls: row.calls,
			lastMinute: row.calls_last_minute,
			bodyBytes: row.body_bytes
		});
	}
	return [...groups.values()].sort((a, b) => a.label.localeCompare(b.label));
}

/**
 * Single gating predicate for both gauge queries: poll only while this section
 * is actually being viewed (the component mounts only for its settings tab)
 * AND the document itself is visible. Non-browser environments have no hidden
 * state, so they default to allowed.
 */
export function isDiagnosticsPollingEnabled(sectionVisible: boolean): boolean {
	if (!sectionVisible) return false;
	if (typeof document === 'undefined') return true;
	return document.visibilityState === 'visible';
}
