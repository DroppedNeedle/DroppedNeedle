/**
 * Smart radio launcher: fetch a track plan, start playback on the first playable
 * track, and hydrate the rest as the station plays.
 *
 * Tiers per track: library file -> native stream (full quality); un-owned ->
 * YouTube full track when the API is configured, else the track is skipped.
 * NOTHING is ever downloaded by radio - all tiers stream. 30s previews are NOT a
 * player tier: they're cross-origin clips the player's Web Audio graph mutes, so
 * preview stations run through the floating widget (deckSampler) instead.
 *
 * Time-to-first-sound: the first request uses `fast: true` (seed-only plan);
 * the full plan is fetched in the background and appended.
 */
import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import { playerStore } from '$lib/stores/player.svelte';
import { radioSession } from '$lib/stores/radioSession.svelte';
import { audioFocus } from '$lib/stores/audioFocus.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import { getCoverUrl } from '$lib/utils/errorHandling';
import {
	needsHydration,
	prepareRadioHydration,
	resolveRadioPatch,
	startRadioHydration
} from '$lib/player/radioQueueHydrator';
import type { QueueItem } from '$lib/player/types';
import type { RadioPlanRequest, RadioPlanResponse, RadioPlanTrack } from '$lib/types';

export type RadioMode = 'library' | 'hybrid';

/** Below this many playable tracks a station is "sparse": it still plays, but
 * the listener gets an honest toast instead of a silent one-track queue. */
const MIN_RICH_STATION = 5;

export interface LaunchRadioOptions {
	shuffle?: boolean;
	mode?: RadioMode;
	count?: number;
}

export function radioTrackKey(track: { artist_name: string; track_name: string }): string {
	return `radio:${track.artist_name.toLowerCase()}|${track.track_name.toLowerCase()}`;
}

/**
 * Map a plan track to a player queue item, or null when it has no player-playable
 * source (un-owned + no YouTube). Nulls are filtered before the queue is built, so
 * the player never carries an item it can't play.
 */
export function planTrackToQueueItem(
	track: RadioPlanTrack,
	ytConfigured: boolean
): QueueItem | null {
	const albumId = track.album_mbid ?? '';
	const coverUrl = albumId ? getCoverUrl(null, albumId) : null;
	if (track.in_library && track.local_file_id) {
		return {
			trackSourceId: track.local_file_id,
			trackName: track.track_name,
			artistName: track.artist_name,
			trackNumber: 0,
			albumId,
			albumName: track.album_name ?? '',
			coverUrl,
			sourceType: 'local',
			artistId: track.artist_mbid || undefined,
			format: track.file_format ?? undefined,
			duration: track.duration_s ?? undefined,
			queueOrigin: 'context',
			playlistTrackId: radioTrackKey(track)
		};
	}
	if (ytConfigured) {
		// placeholder: the hydrator fills trackSourceId with a video id just-in-time
		return {
			trackSourceId: '',
			trackName: track.track_name,
			artistName: track.artist_name,
			trackNumber: 0,
			albumId,
			albumName: track.album_name ?? '',
			coverUrl,
			sourceType: 'youtube',
			artistId: track.artist_mbid || undefined,
			queueOrigin: 'context',
			playlistTrackId: radioTrackKey(track)
		};
	}
	// un-owned and no YouTube: nothing to play in the main player (previews live
	// in the widget, not here) -> drop it
	return null;
}

async function fetchPlan(request: RadioPlanRequest): Promise<RadioPlanResponse> {
	return api.global.post<RadioPlanResponse>(API.discoverRadioPlan(), request);
}

/** Keep each freshly-claimed key exactly once (a plan can contain duplicates). */
function pickFresh(
	tracks: RadioPlanTrack[],
	fresh: { artist_name: string; track_name: string }[]
): RadioPlanTrack[] {
	const remaining = new Set(fresh.map((t) => radioTrackKey(t)));
	const out: RadioPlanTrack[] = [];
	for (const t of tracks) {
		const key = radioTrackKey(t);
		if (remaining.has(key)) {
			remaining.delete(key);
			out.push(t);
		}
	}
	return out;
}

export async function launchRadio(
	seed: Omit<RadioPlanRequest, 'exclude_recording_mbids' | 'fast'>,
	ytConfigured: boolean,
	options: LaunchRadioOptions = {}
): Promise<boolean> {
	const requestedMode: RadioMode = options.mode ?? seed.mode ?? 'hybrid';
	// Without YouTube the player can't stream un-owned tracks (they'd all be
	// dropped from the queue), so every station degrades to a library-first
	// plan the backend fills from the user's own files.
	const mode: RadioMode = ytConfigured ? requestedMode : 'library';
	const effectiveYt = ytConfigured;
	const request = { ...seed, mode, count: options.count ?? seed.count ?? 30 };

	// never talk over the deck (one-sound rule): starting a station owns audio
	audioFocus.interrupt();

	let fastPlan: RadioPlanResponse;
	try {
		fastPlan = await fetchPlan({ ...request, fast: true });
	} catch {
		playbackToast.show("Couldn't tune this station", 'error');
		return false;
	}
	if (fastPlan.tracks.length === 0) {
		playbackToast.show(
			mode === 'library'
				? 'Nothing in your library for this yet - try the full-tracks mode'
				: 'Nothing to play for this station yet',
			'warning'
		);
		return false;
	}

	radioSession.start(fastPlan.title, request);
	const fresh = radioSession.claim(
		fastPlan.tracks.map((t) => ({
			artist_name: t.artist_name,
			track_name: t.track_name,
			recording_mbid: t.recording_mbid
		}))
	);
	const items = pickFresh(fastPlan.tracks, fresh)
		.map((t) => planTrackToQueueItem(t, effectiveYt))
		.filter((item): item is QueueItem => item !== null);

	// The player advances through unplayable items in milliseconds - far faster
	// than the background hydrator - so the FIRST track must be playable before
	// playback starts, or three placeholder misses kill the station instantly.
	prepareRadioHydration(effectiveYt);
	while (items.length > 0 && needsHydration(items[0])) {
		const patch = await resolveRadioPatch(items[0]);
		if (patch) {
			items[0] = { ...items[0], ...patch };
			break;
		}
		items.shift(); // no playable source for this track: drop it
	}
	if (items.length === 0) {
		radioSession.end();
		playbackToast.show('Nothing playable for this station right now', 'warning');
		return false;
	}

	playerStore.playQueue(items, 0, options.shuffle ?? false);
	startRadioHydration();

	// full plan in the background; append what the fast plan didn't cover.
	// Once it lands, a still-tiny queue means the station genuinely has almost
	// nothing to play - say so instead of silently looping one track.
	void extendRadio(effectiveYt).then(() => {
		if (!radioSession.active) return;
		const playable = playerStore.queue.length;
		if (playable > 0 && playable < MIN_RICH_STATION) {
			playbackToast.show(
				`Station found only ${playable} ${playable === 1 ? 'match' : 'matches'}` +
					(effectiveYt ? '' : ' - add more music or connect YouTube'),
				'warning'
			);
		}
	});
	return true;
}

/** Fetch more tracks for the live session and append them to the player queue. */
export async function extendRadio(ytConfigured: boolean): Promise<void> {
	const seed = radioSession.seed;
	if (!radioSession.active || !seed || radioSession.extending || radioSession.atCapacity) return;
	radioSession.extending = true;
	try {
		const plan = await fetchPlan({
			...seed,
			exclude_recording_mbids: radioSession.exclusions,
			fast: false
		});
		if (!radioSession.active) return;
		const fresh = radioSession.claim(
			plan.tracks.map((t) => ({
				artist_name: t.artist_name,
				track_name: t.track_name,
				recording_mbid: t.recording_mbid
			}))
		);
		const items = pickFresh(plan.tracks, fresh)
			.map((t) => planTrackToQueueItem(t, ytConfigured))
			.filter((item): item is QueueItem => item !== null);
		if (items.length > 0) {
			playerStore.addMultipleToQueue(items);
		}
	} catch {
		// extension is best-effort; the current queue keeps playing
	} finally {
		radioSession.extending = false;
	}
}
