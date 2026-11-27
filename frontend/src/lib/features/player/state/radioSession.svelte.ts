import { SvelteSet } from 'svelte/reactivity';
import type { RadioPlanRequest } from '$lib/features/discover/queries/types';
import type { components } from '$lib/api/v3/openapi';

/** One of the station's tracks, as a plan request names it. */
export type StationTrack = components['schemas']['StationTrack'];
import type { PlayedFrom } from '$lib/features/player/playback/types';

/** What a playing station asks the server for again: the server keeps no station. */
export interface Station {
	request: Omit<RadioPlanRequest, 'station_tracks' | 'more_like' | 'not_this'>;
	ytConfigured: boolean;
	playedFrom: PlayedFrom;
}

/** How an ask for the station's next tracks ended. */
export type AskOutcome = 'added' | 'nothing_new' | 'failed';

/** The most tracks of each kind the station remembers, as the server takes. */
const PLAYED_KEPT = 500;
/** The most steering tracks of each kind, as the server takes; the oldest go first. */
const STEERING_KEPT = 50;
/** The first wait after a failed ask, doubled each time up to the longest. */
const RETRY_FIRST_MS = 5_000;
const RETRY_LONGEST_MS = 60_000;

function keepLast(list: string[], most: number) {
	if (list.length > most) list.splice(0, list.length - most);
}

function createRadioSession() {
	let active = $state(false);
	let generation = 0;
	let controller: AbortController | null = null;
	let station: Station | null = null;
	let ask = 0;
	let asking = false;
	let exhausted = false;
	let failures = 0;
	let retryAt = 0;
	let moreLike: string[] = [];
	let notThis: string[] = [];
	let played: StationTrack[] = [];
	const noted = new SvelteSet<string>();

	function reset() {
		station = null;
		asking = false;
		exhausted = false;
		failures = 0;
		retryAt = 0;
		moreLike = [];
		notThis = [];
		played = [];
		noted.clear();
	}

	return {
		get active() {
			return active;
		},

		/** The playing station, or null when none plays. */
		get station() {
			return active ? station : null;
		},

		/** Tracks the person asked for more like on this station. */
		get moreLike() {
			return moreLike;
		},

		/** Tracks the person said "Not this" to on this station. */
		get notThis() {
			return notThis;
		},

		/** The station's tracks played so far, in the order they played. */
		get played() {
			return played;
		},

		/** Whether the station had nothing more to give. */
		get exhausted() {
			return exhausted;
		},

		beginLaunch() {
			controller?.abort();
			controller = new AbortController();
			active = false;
			generation += 1;
			reset();
			return { generation, signal: controller.signal };
		},

		isCurrent(candidate: number) {
			return candidate === generation;
		},

		start(candidate: number, playing?: Station) {
			if (candidate !== generation) return false;
			controller = null;
			station = playing ?? null;
			active = true;
			return true;
		},

		/** Note a station track that played, once however often it is noted. */
		notePlaying(key: string, track: StationTrack) {
			if (!active || noted.has(key)) return;
			noted.add(key);
			played.push(track);
			if (played.length > PLAYED_KEPT) played.splice(0, played.length - PLAYED_KEPT);
		},

		/**
		 * Starts asking for the station's next tracks, or null while one ask is
		 * out, a failed one waits to be tried again or no station plays. `steer`
		 * aborts the ask out and asks again. Ending the station aborts the ask,
		 * so a late answer can never refill a queue the player cleared.
		 */
		beginExtend(steer = false) {
			if (!active || !station) return null;
			if (!steer && (asking || exhausted || Date.now() < retryAt)) return null;
			controller?.abort();
			controller = new AbortController();
			asking = true;
			ask += 1;
			return { generation, ask, signal: controller.signal };
		},

		/** Whether this ask is still the one the station waits on. */
		isCurrentAsk(candidate: { generation: number; ask: number }) {
			return active && candidate.generation === generation && candidate.ask === ask;
		},

		finishExtend(candidate: { generation: number; ask: number }, outcome: AskOutcome) {
			if (candidate.generation !== generation || candidate.ask !== ask) return;
			asking = false;
			controller = null;
			if (outcome === 'failed') {
				failures += 1;
				retryAt = Date.now() + Math.min(RETRY_FIRST_MS * 2 ** (failures - 1), RETRY_LONGEST_MS);
				return;
			}
			failures = 0;
			retryAt = 0;
			if (outcome === 'nothing_new') exhausted = true;
		},

		/** How many asks in a row have failed. */
		get failures() {
			return failures;
		},

		/** Steer the station by one library track; it lasts as long as the station. */
		steer(kind: 'more_like' | 'not_this', trackId: string) {
			if (!active) return;
			const list = kind === 'more_like' ? moreLike : notThis;
			if (!list.includes(trackId)) list.push(trackId);
			keepLast(list, STEERING_KEPT);
			exhausted = false;
		},

		end() {
			controller?.abort();
			controller = null;
			generation += 1;
			active = false;
			reset();
		}
	};
}

export const radioSession = createRadioSession();
