/**
 * Loads one finite radio plan. Local files stream natively; unowned tracks use
 * YouTube when configured. Cross-origin previews stay in deckSampler because the
 * player's Web Audio graph mutes them.
 */
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { DiscoverV3Api } from '$lib/queries/discover/DiscoverV3Api';
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

export interface LaunchRadioOptions {
	shuffle?: boolean;
	mode?: RadioMode;
	count?: number;
}

export function radioTrackKey(track: { artist_name: string; track_name: string }): string {
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
			playlistTrackId: radioTrackKey(track)
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
			queueOrigin: 'context',
			playlistTrackId: radioTrackKey(track)
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
		duration_s: track.duration_s ?? null
	};
}

async function fetchPlan(
	request: RadioPlanRequest & { mode: RadioMode },
	signal: AbortSignal
): Promise<RadioPlanResponse> {
	const plan = await api.global.v3.POST(DiscoverV3Api.radioPlan(), request, { signal });
	return { title: plan.title, tracks: (plan.tracks ?? []).map(toPlanTrack) };
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
	seed: Omit<RadioPlanRequest, 'exclude_recording_mbids'>,
	ytConfigured: boolean,
	options: LaunchRadioOptions = {}
): Promise<boolean> {
	const mode: RadioMode = options.mode ?? seed.mode ?? 'hybrid';
	const effectiveYt = ytConfigured;
	const request = { ...seed, mode, count: options.count ?? seed.count ?? 30 };
	const launch = radioSession.beginLaunch();

	// starting a station takes audio focus from the preview player
	audioFocus.interrupt();

	let plan: RadioPlanResponse;
	try {
		plan = await fetchPlan(request, launch.signal);
	} catch {
		if (!radioSession.isCurrent(launch.generation)) return false;
		radioSession.end();
		playbackToast.show("Couldn't tune this station", 'error');
		return false;
	}
	if (!radioSession.isCurrent(launch.generation)) return false;
	if (plan.tracks.length === 0) {
		radioSession.end();
		playbackToast.show(
			mode === 'library'
				? 'Nothing in your library for this yet - try the full-tracks mode'
				: 'Nothing to play for this station yet',
			'warning'
		);
		return false;
	}

	const items = dedupeTracks(plan.tracks)
		.map((t) => planTrackToQueueItem(t, effectiveYt))
		.filter((item): item is QueueItem => item !== null);

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
		playbackToast.show('Nothing playable for this station right now', 'warning');
		return false;
	}

	if (!radioSession.start(launch.generation)) return false;
	playerStore.playQueue(items, 0, options.shuffle ?? false);
	startRadioHydration();
	return true;
}
