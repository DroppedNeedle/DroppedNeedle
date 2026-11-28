/**
 * Plays a Daily Mix: every track in the mix's stored order. A library track
 * streams from the library; a new one plays through YouTube while it is set
 * up, its video found just before it plays, as a station's are, and is left
 * out when none is found.
 */
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import { playerStore } from '$lib/features/player/state/player.svelte';
import { toastStore } from '$lib/stores/toast';
import { recordingFor } from '$lib/features/player/playback/queueHelpers';
import { getCoverUrl } from '$lib/utils/errorHandling';
import type { QueueItem } from '$lib/features/player/playback/types';
import type { DailyMix } from '$lib/features/discover/queries/types';

/** Tracks ahead of the one playing whose videos are found. */
const LOOKAHEAD = 3;
const TICK_MS = 2000;
const KEY = 'mix:';

let timer: ReturnType<typeof setInterval> | null = null;
/** Counts Play presses, so a late video search knows a newer one came. */
let launches = 0;
const finding = new Set<string>();

/** The mix's tracks as queue items, in its order: new ones only with YouTube set up. */
export function mixQueue(mix: DailyMix, ytConfigured: boolean): QueueItem[] {
	return mix.tracks.flatMap((track, position): QueueItem[] => {
		const albumId = track.album.musicbrainz_id ?? track.album.local_id ?? '';
		const common = {
			trackName: track.title,
			artistName: track.artist_name,
			trackNumber: 0,
			albumId,
			albumName: track.album.title,
			coverUrl: getCoverUrl(null, track.album.musicbrainz_id ?? ''),
			artistId: track.artist_mbid ?? undefined,
			queueOrigin: 'context' as const,
			playlistTrackId: `${KEY}${mix.id}:${position}`,
			playedFrom: { kind: 'mix' as const, id: mix.id }
		};
		if (track.kind !== 'new' && track.local_track_id) {
			return [
				{
					...common,
					trackSourceId: track.local_track_id,
					sourceType: 'local',
					format: track.file_format ?? undefined,
					duration: track.duration_seconds ?? undefined,
					availableSources: ['local']
				}
			];
		}
		if (track.kind === 'new' && ytConfigured) {
			return [
				{
					...common,
					trackSourceId: '',
					sourceType: 'youtube',
					recordingIds: recordingFor('youtube', track.recording_mbid)
				}
			];
		}
		return [];
	});
}

function needsVideo(item: QueueItem): boolean {
	return (
		!!item.playlistTrackId?.startsWith(KEY) && item.sourceType === 'youtube' && !item.trackSourceId
	);
}

/** The video for a new track, or null when none is found. */
async function videoOf(item: QueueItem): Promise<string | null> {
	try {
		const found = await api.v3.GET(
			v3('/api/v3/discover/queue/youtube-track-search', {
				query: { artist: item.artistName, track: item.trackName }
			})
		);
		return found.video_id || null;
	} catch {
		return null;
	}
}

function stop(): void {
	if (timer) clearInterval(timer);
	timer = null;
}

async function find(item: QueueItem): Promise<void> {
	const key = item.playlistTrackId ?? '';
	if (finding.has(key)) return;
	finding.add(key);
	try {
		const video = await videoOf(item);
		if (video) {
			playerStore.patchQueueItemByPlaylistTrackId(key, { trackSourceId: video });
			return;
		}
		const index = playerStore.queue.findIndex((q: QueueItem) => q.playlistTrackId === key);
		if (index >= 0 && index !== playerStore.currentIndex) playerStore.removeFromQueue(index);
	} finally {
		finding.delete(key);
	}
}

async function tick(): Promise<void> {
	const queue = playerStore.queue as QueueItem[];
	if (!queue.some((item) => item.playlistTrackId?.startsWith(KEY))) {
		stop();
		return;
	}
	const from = playerStore.currentIndex;
	await Promise.all(
		queue
			.slice(from, from + 1 + LOOKAHEAD)
			.filter(needsVideo)
			.map(find)
	);
}

/** Play `mix` from its first track that can play. */
export async function playMix(mix: DailyMix, ytConfigured: boolean): Promise<void> {
	const launch = ++launches;
	const playing = playerStore.queue;
	const items = mixQueue(mix, ytConfigured);
	while (items.length > 0 && needsVideo(items[0])) {
		const video = await videoOf(items[0]);
		// A later Play, or anything the person started meanwhile, wins.
		if (launch !== launches || playerStore.queue !== playing) return;
		if (video) {
			items[0] = { ...items[0], trackSourceId: video };
			break;
		}
		items.shift();
	}
	if (items.length === 0) {
		toastStore.notify('Nothing playable for this mix right now', 'warning');
		return;
	}
	playerStore.playQueue(items, 0, false);
	stop();
	timer = setInterval(() => void tick(), TICK_MS);
}
