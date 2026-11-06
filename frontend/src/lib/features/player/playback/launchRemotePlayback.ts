import { playerStore } from '$lib/features/player/state/player.svelte';
import type { PlaybackMeta } from '$lib/features/player/playback/types';
import type { RemoteTrack } from '$lib/features/remote-library/queries/types';
import { buildRemoteQueueItems, remoteStreamKey } from '$lib/features/player/playback/queueHelpers';

/**
 * Plays an album's remote tracks from `startIndex`. Tracks that cannot stream
 * are skipped, and if the chosen one cannot, play starts at the first that can.
 */
export function launchRemotePlayback(
	tracks: RemoteTrack[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	const items = buildRemoteQueueItems(tracks, meta);
	if (!items.length) return;
	const chosen = tracks[startIndex];
	const startKey = chosen ? remoteStreamKey(chosen) : null;
	const startAt = startKey ? items.findIndex((item) => item.trackSourceId === startKey) : -1;

	playerStore.playQueue(items, Math.max(startAt, 0), shuffle);
}
