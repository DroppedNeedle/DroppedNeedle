import { playerStore } from '$lib/stores/player.svelte';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';
import type { JellyfinTrackInfo } from '$lib/player/types';
import { getCoverUrl } from '$lib/utils/errorHandling';
import { normalizeCodec } from '$lib/player/queueHelpers';

export function launchJellyfinPlayback(
	tracks: JellyfinTrackInfo[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const items: QueueItem[] = tracks.map((t) => {
		const format = normalizeCodec(t.codec);
		return {
			trackSourceId: t.jellyfin_id,
			trackName: t.title,
			artistName: meta.artistName,
			trackNumber: t.track_number,
			discNumber: t.disc_number ?? 1,
			albumId: meta.albumId,
			albumName: meta.albumName,
			coverUrl: normalizedCoverUrl,
			sourceType: 'jellyfin' as const,
			artistId: meta.artistId,
			streamUrl: gatewayStreamUrl('jellyfin', t.jellyfin_id),
			format
		};
	});

	playerStore.playQueue(items, startIndex, shuffle);
}
