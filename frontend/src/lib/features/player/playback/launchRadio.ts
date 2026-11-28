import { v3 } from '$lib/api/v3/endpoint';
/**
 * Plays a radio station: its first tracks, then the next chunk each time few
 * are left, sending what the station played and how the person steered it (the
 * server keeps no station). Local files stream natively; unowned tracks use
 * YouTube when configured. Cross-origin previews stay in deckSampler because the
 * player's Web Audio graph mutes them.
 */
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { playerStore } from '$lib/features/player/state/player.svelte';
import {
	radioSession,
	type AskOutcome,
	type StationTrack
} from '$lib/features/player/state/radioSession.svelte';
import { upcomingIndexes } from '$lib/features/player/state/playerQueueOps';
import { audioFocus } from '$lib/features/player/state/audioFocus.svelte';
import { toastStore } from '$lib/stores/toast';
import { readIntegrationStatus } from '$lib/features/home/queries/home.svelte';
import { getCoverUrl } from '$lib/utils/errorHandling';
import {
	needsHydration,
	prepareRadioHydration,
	resolveRadioPatch,
	startRadioHydration
} from '$lib/features/player/playback/radioQueueHydrator';
import { recordingFor, withPlayedFrom } from '$lib/features/player/playback/queueHelpers';
import type { PlayedFrom, QueueItem } from '$lib/features/player/playback/types';
import type {
	RadioPlanRequest,
	RadioPlanResponse,
	RadioPlanTrack
} from '$lib/features/discover/queries/types';

export type RadioMode = 'library' | 'hybrid';

/** Tracks a station starts with. */
const FIRST_CHUNK = 30;
/** Tracks each later ask brings. */
export const NEXT_CHUNK = 15;
/** The station asks for more when this many tracks are left to play. */
export const ASK_WHEN_LEFT = 5;
/** The most of the station's tracks one ask sends, newest kept. */
const SENT_MOST = 500;

/** What a station starts from; what it played and its steering are added as it plays. */
export type StationSeed = Omit<RadioPlanRequest, 'station_tracks' | 'more_like' | 'not_this'>;

export interface LaunchRadioOptions {
	shuffle?: boolean;
	mode?: RadioMode;
	count?: number;
	/** What the station's plays say they came from; the station itself unless told. */
	playedFrom?: PlayedFrom;
}

/** A station is named by its seed (`artist:<mbid>`, `genre:<name>`); one made of items is not. */
function stationPlayedFrom(seed: StationSeed): PlayedFrom {
	return seed.seed_id
		? { kind: 'radio', id: `${seed.seed_type}:${seed.seed_id}` }
		: { kind: 'radio' };
}

function radioTrackKey(track: { artist_name: string; track_name: string }): string {
	return `radio:${track.artist_name.toLowerCase()}|${track.track_name.toLowerCase()}`;
}

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
			playlistTrackId: radioTrackKey(track),
			reason: track.reason ?? null
		};
	}
	if (ytConfigured) {
		// the hydrator resolves the video just before playback
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
			recordingIds: recordingFor('youtube', track.recording_mbid),
			queueOrigin: 'context',
			playlistTrackId: radioTrackKey(track),
			reason: track.reason ?? null
		};
	}
	return null;
}

function toPlanTrack(track: components['schemas']['RadioPlanTrack']): RadioPlanTrack {
	return {
		track_name: track.track_name,
		artist_name: track.artist_name,
		artist_mbid: track.artist_mbid ?? '',
		recording_mbid: track.recording_mbid ?? null,
		album_mbid: track.album_mbid ?? null,
		album_name: track.album_name ?? null,
		in_library: track.in_library ?? false,
		local_file_id: track.local_file_id ?? null,
		file_format: track.file_format ?? null,
		duration_s: track.duration_s ?? null,
		reason: track.reason ?? null
	};
}

/** The stations already told they play the library only. */
const toldLibraryOnly = new WeakSet<object>();

/**
 * Says once a station that asked for tracks the library lacks plays the
 * library only, because no source is set up to play the rest.
 */
function tellLibraryOnly(station: object, plan: RadioPlanResponse): void {
	if (!plan.library_only || toldLibraryOnly.has(station)) return;
	toldLibraryOnly.add(station);
	toastStore.notify(
		'This station plays your library only: nothing is set up to play tracks you do not have.',
		'info'
	);
}

async function fetchPlan(
	request: RadioPlanRequest,
	signal: AbortSignal
): Promise<RadioPlanResponse> {
	const plan = await api.v3.POST(v3('/api/v3/discover/radio/plan'), request, { signal });
	return {
		title: plan.title,
		tracks: (plan.tracks ?? []).map(toPlanTrack),
		library_only: plan.library_only ?? false
	};
}

/** Whether `item` is one of a station's own tracks, not one queued beside it. */
export function isStationItem(item: QueueItem): boolean {
	return !!item.playlistTrackId?.startsWith('radio:');
}

function libraryId(item: QueueItem): string | null {
	return item.sourceType === 'local' && item.trackSourceId ? item.trackSourceId : null;
}

function stationTrack(item: QueueItem): StationTrack {
	return {
		local_track_id: libraryId(item),
		recording_mbid: item.recordingIds?.youtube ?? null,
		artist_mbid: item.artistId ?? '',
		artist_name: item.artistName,
		track_name: item.trackName
	};
}

/**
 * Notes the station tracks played so far, in play order: the one playing and
 * those the queue still holds before it, so a track skipped between two ticks
 * is noted too. The queue keeps only a few played tracks; the session keeps
 * the rest.
 */
export function notePlaying(): void {
	const queue = playerStore.queue as QueueItem[];
	const current = playerStore.currentIndex;
	if (!queue[current]) return;
	const playedIndexes = playerStore.shuffleEnabled
		? playerStore.shuffleOrder.slice(0, Math.max(0, playerStore.shuffleOrder.indexOf(current)) + 1)
		: Array.from({ length: current + 1 }, (_, index) => index);
	for (const index of playedIndexes) {
		const item = queue[index];
		if (item && isStationItem(item) && item.playlistTrackId) {
			radioSession.notePlaying(item.playlistTrackId, stationTrack(item));
		}
	}
}

/** The station's tracks still to play, in play order. */
function upcomingStationItems(): { index: number; item: QueueItem }[] {
	const queue = playerStore.queue as QueueItem[];
	return upcomingIndexes(
		queue.length,
		playerStore.currentIndex,
		playerStore.shuffleEnabled,
		playerStore.shuffleOrder
	)
		.map((index) => ({ index, item: queue[index] }))
		.filter(({ item }) => item && isStationItem(item));
}

/** The station's tracks in play order: those played, then those still to play. */
function stationSoFar(upcoming: QueueItem[]): { station_tracks: StationTrack[] } {
	return {
		station_tracks: [...radioSession.played, ...upcoming.map(stationTrack)].slice(-SENT_MOST)
	};
}

/**
 * Asks for the station's next tracks once few are left, and queues them. A
 * station that stopped, changed or was cleared while the ask was out takes
 * nothing from its answer, so a late answer never starts playback again. A
 * failed ask is tried again by the tick after a wait; only an answer with
 * nothing new ends the station. `replace` (a steer) asks at once, aborting an
 * ask already out, and swaps the station's tracks still to play for the answer.
 */
export async function extendRadio(replace = false): Promise<boolean> {
	const station = radioSession.station;
	if (!station) return false;
	notePlaying();
	if (!replace && playerStore.upcomingQueueLength > ASK_WHEN_LEFT) return false;
	const turn = radioSession.beginExtend(replace);
	if (!turn) return false;
	let outcome: AskOutcome = 'failed';
	try {
		const kept = replace ? [] : upcomingStationItems().map(({ item }) => item);
		const plan = await fetchPlan(
			{
				...station.request,
				count: NEXT_CHUNK,
				...stationSoFar(kept),
				more_like: [...radioSession.moreLike],
				not_this: [...radioSession.notThis]
			},
			turn.signal
		);
		if (!radioSession.isCurrentAsk(turn)) return false;
		if ((playerStore.queue as QueueItem[]).length === 0) return false;
		tellLibraryOnly(station, plan);
		const replaced = replace ? upcomingStationItems() : [];
		const going = new Set(replaced.map(({ index }) => index));
		const inQueue = new Set(
			(playerStore.queue as QueueItem[])
				.filter((_, index) => !going.has(index))
				.map((item) => item.playlistTrackId)
		);
		const items = withPlayedFrom(
			dedupeTracks(plan.tracks)
				.map((t) => planTrackToQueueItem(t, station.ytConfigured))
				.filter((item): item is QueueItem => item !== null && !inQueue.has(item.playlistTrackId)),
			station.playedFrom
		);
		// A steer that brings nothing new keeps what was queued, and the
		// station goes on.
		if (replace && items.length === 0) {
			outcome = 'added';
			return false;
		}
		for (const { index } of replaced.sort((a, b) => b.index - a.index)) {
			playerStore.removeFromQueue(index);
		}
		playerStore.appendQueueSilent(items);
		outcome = items.length > 0 ? 'added' : 'nothing_new';
		return items.length > 0;
	} catch (error) {
		if (radioSession.isCurrentAsk(turn) && radioSession.failures === 0) {
			console.warn('The station could not get its next tracks; it tries again shortly', error);
		}
		return false;
	} finally {
		radioSession.finishExtend(turn, outcome);
	}
}

/**
 * Steers the playing station by one of its library tracks: "More like this" grows
 * it from the track as well, "Not this" skips the track and keeps it and what
 * sounds like it away. Either lasts for the station and, once the new answer is
 * in, replaces what the station has left to play; if the ask fails, what was
 * queued stays.
 */
export async function steerRadio(kind: 'more_like' | 'not_this', item: QueueItem): Promise<void> {
	const id = libraryId(item);
	if (!radioSession.active || !id) return;
	radioSession.steer(kind, id);
	if (kind === 'not_this') playerStore.nextTrack();
	await extendRadio(true);
}

/** What the playing station grew from, or null when none plays. */
export function playingStation(): Pick<StationSeed, 'seed_type' | 'seed_id'> | null {
	const station = radioSession.station;
	return station
		? { seed_type: station.request.seed_type, seed_id: station.request.seed_id }
		: null;
}

function dedupeTracks(tracks: RadioPlanTrack[]): RadioPlanTrack[] {
	const seen = new Set<string>();
	const out: RadioPlanTrack[] = [];
	for (const track of tracks) {
		const key = radioTrackKey(track);
		if (seen.has(key)) continue;
		seen.add(key);
		out.push(track);
	}
	return out;
}

export async function launchRadio(
	seed: StationSeed,
	ytConfigured: boolean,
	options: LaunchRadioOptions = {}
): Promise<boolean> {
	const mode: RadioMode = options.mode ?? seed.mode ?? 'hybrid';
	const effectiveYt = ytConfigured;
	const request = { ...seed, mode, count: options.count ?? seed.count ?? FIRST_CHUNK };
	const playedFrom = options.playedFrom ?? stationPlayedFrom(seed);
	const launch = radioSession.beginLaunch();

	// starting a station takes audio focus from the preview player
	audioFocus.interrupt();

	let plan: RadioPlanResponse;
	try {
		plan = await fetchPlan(request, launch.signal);
	} catch {
		if (!radioSession.isCurrent(launch.generation)) return false;
		radioSession.end();
		toastStore.notify("Couldn't tune this station", 'error');
		return false;
	}
	if (!radioSession.isCurrent(launch.generation)) return false;
	if (plan.tracks.length === 0) {
		radioSession.end();
		toastStore.notify(
			mode === 'library'
				? 'Nothing in your library for this yet - try the full-tracks mode'
				: 'Nothing to play for this station yet',
			'warning'
		);
		return false;
	}

	const items = withPlayedFrom(
		dedupeTracks(plan.tracks)
			.map((t) => planTrackToQueueItem(t, effectiveYt))
			.filter((item): item is QueueItem => item !== null),
		playedFrom
	);

	// resolve the head inline or rapid placeholder failures can stop the station
	prepareRadioHydration();
	while (items.length > 0 && needsHydration(items[0])) {
		const patch = await resolveRadioPatch(items[0]);
		if (!radioSession.isCurrent(launch.generation)) return false;
		if (patch) {
			items[0] = { ...items[0], ...patch };
			break;
		}
		items.shift();
	}
	if (items.length === 0) {
		radioSession.end();
		toastStore.notify('Nothing playable for this station right now', 'warning');
		return false;
	}

	const station = { request: { ...seed, mode }, ytConfigured: effectiveYt, playedFrom };
	if (!radioSession.start(launch.generation, station)) return false;
	playerStore.playQueue(items, 0, options.shuffle ?? false);
	tellLibraryOnly(radioSession.station ?? station, plan);
	startRadioHydration(() => void extendRadio());
	notePlaying();
	return true;
}

/**
 * Starts a station from a page's "Start radio": an artist, an album, a library
 * track or a playlist. It plays tracks the library lacks through YouTube when
 * that is set up.
 */
export function startRadio(seed: Pick<StationSeed, 'seed_type' | 'seed_id'>): Promise<boolean> {
	return launchRadio(seed, !!readIntegrationStatus()?.youtube_api);
}
