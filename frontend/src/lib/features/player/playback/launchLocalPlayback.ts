import { playerStore } from '$lib/features/player/state/player.svelte';
import type { LocalTrackInfo, PlaybackMeta } from '$lib/features/player/playback/types';
import { buildQueueItemsFromLocal } from '$lib/features/player/playback/queueHelpers';

export function launchLocalPlayback(
	tracks: LocalTrackInfo[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	playerStore.playQueue(buildQueueItemsFromLocal(tracks, meta), startIndex, shuffle);
}
