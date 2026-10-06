/**
 * View logic for the admin Diagnostics settings card: labels, the slot
 * gauges and the polling gate.
 */
import type { components } from '$lib/api/v3/openapi';

type SlotView = components['schemas']['SlotView'];

export const PROVIDER_LABELS: Readonly<Record<string, string>> = {
	musicbrainz: 'MusicBrainz',
	listenbrainz: 'ListenBrainz',
	lastfm: 'Last.fm',
	coverart: 'Cover Art Archive',
	audiodb: 'AudioDB',
	discogs: 'Discogs'
};

export const LANE_LABELS: Readonly<Record<string, string>> = {
	user: 'User requests',
	image: 'Artwork',
	background: 'Background'
};

/** 'user_initiated' -> 'User initiated'; unknown wire values stay readable. */
export function humanizeWireValue(value: string): string {
	const spaced = value.replaceAll('_', ' ').trim();
	if (!spaced) return value;
	return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

export function providerLabel(provider: string): string {
	return PROVIDER_LABELS[provider] ?? humanizeWireValue(provider);
}

export function laneLabel(lane: string): string {
	return LANE_LABELS[lane] ?? humanizeWireValue(lane);
}

export function formatCount(count: number): string {
	return count.toLocaleString('en-US');
}

export interface QueueLaneView {
	key: 'user' | 'image' | 'background';
	label: string;
	slotsAvailable: number;
	active: boolean | null;
	waiting: number | null;
}

/** One gauge cell per priority lane; user_active rides the user lane,
 * background_waiters the background lane. */
export function buildQueueLanes(slots: SlotView): QueueLaneView[] {
	return [
		{
			key: 'user',
			label: laneLabel('user'),
			slotsAvailable: slots.user_slots_available,
			active: slots.user_active,
			waiting: null
		},
		{
			key: 'image',
			label: laneLabel('image'),
			slotsAvailable: slots.image_slots_available,
			active: null,
			waiting: null
		},
		{
			key: 'background',
			label: laneLabel('background'),
			slotsAvailable: slots.background_slots_available,
			active: null,
			waiting: slots.background_waiters
		}
	];
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
