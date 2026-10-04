import { playerStore } from '$lib/stores/player.svelte';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import type { LocalTrackInfo, PlaybackMeta, QueueItem } from '$lib/player/types';
import type { TrackViewV3 } from '$lib/queries/library/LibraryV3Queries.svelte';
import { getCoverUrl } from '$lib/utils/errorHandling';

export function launchLocalPlayback(
	tracks: LocalTrackInfo[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const items: QueueItem[] = tracks.map((t) => ({
		trackSourceId: String(t.track_file_id),
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number,
		discNumber: t.disc_number ?? 1,
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		coverRemoteUrl: meta.coverUrl?.startsWith('http') ? meta.coverUrl : null,
		sourceType: 'local',
		artistId: meta.artistId,
		streamUrl: gatewayStreamUrl('local', t.track_file_id),
		format: t.format.toLowerCase()
	}));

	playerStore.playQueue(items, startIndex, shuffle);
}

// v3 input variant. Same playback as launchLocalPlayback, reading
// TrackViewV3 (track_file_id becomes id) instead of LocalTrackInfo.
export function launchLocalPlaybackV3(
	tracks: TrackViewV3[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const items: QueueItem[] = tracks.map((t) => ({
		trackSourceId: t.id,
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number,
		discNumber: t.disc_number ?? 1,
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		coverRemoteUrl: meta.coverUrl?.startsWith('http') ? meta.coverUrl : null,
		sourceType: 'local',
		artistId: meta.artistId,
		streamUrl: gatewayStreamUrl('local', t.id),
		format: t.format.toLowerCase()
	}));

	playerStore.playQueue(items, startIndex, shuffle);
}
