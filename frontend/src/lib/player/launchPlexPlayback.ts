import { playerStore } from '$lib/stores/player.svelte';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';
import type { PlexTrackInfo } from '$lib/player/types';
import type { components } from '$lib/api/v3/openapi';
import { getCoverUrl } from '$lib/utils/errorHandling';
import { normalizeCodec, normalizeDiscNumber } from '$lib/player/queueHelpers';

type RemotesTrackView = components['schemas']['RemotesTrackView'];

export function launchPlexPlayback(
	tracks: PlexTrackInfo[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const selectedTrack = tracks[startIndex];
	const streamable = tracks.filter((t) => t.part_key);
	if (!streamable.length) return;

	let adjustedIndex = 0;
	if (selectedTrack?.part_key) {
		const found = streamable.indexOf(selectedTrack);
		adjustedIndex = found >= 0 ? found : 0;
	}

	const items: QueueItem[] = streamable.map((t) => {
		const format = normalizeCodec(t.codec);
		return {
			trackSourceId: t.part_key!,
			trackName: t.title,
			artistName: meta.artistName,
			trackNumber: t.track_number,
			discNumber: normalizeDiscNumber(t.disc_number),
			albumId: meta.albumId,
			albumName: meta.albumName,
			coverUrl: normalizedCoverUrl,
			sourceType: 'plex' as const,
			artistId: meta.artistId,
			streamUrl: gatewayStreamUrl('plex', t.part_key!),
			format,
			plexRatingKey: t.plex_id
		};
	});

	playerStore.playQueue(items, adjustedIndex, shuffle);
}

// v3 input variant. Same playback as launchPlexPlayback, reading
// RemotesTrackView (source plex) instead of PlexTrackInfo. The v3 id
// is the ratingKey (scrobble key); streaming uses part_key, and
// tracks without one are dropped like the v1 launcher.
export function launchPlexPlaybackV3(
	tracks: RemotesTrackView[],
	startIndex: number = 0,
	shuffle: boolean = false,
	meta: PlaybackMeta
): void {
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const selectedTrack = tracks[startIndex];
	const streamable = tracks.filter((t) => t.part_key);
	if (!streamable.length) return;

	let adjustedIndex = 0;
	if (selectedTrack?.part_key) {
		const found = streamable.indexOf(selectedTrack);
		adjustedIndex = found >= 0 ? found : 0;
	}

	const items: QueueItem[] = streamable.map((t) => ({
		trackSourceId: t.part_key!,
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number ?? 0,
		discNumber: normalizeDiscNumber(t.disc_number),
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		sourceType: 'plex' as const,
		artistId: meta.artistId,
		streamUrl: gatewayStreamUrl('plex', t.part_key!),
		format: normalizeCodec(undefined),
		plexRatingKey: t.id
	}));

	playerStore.playQueue(items, adjustedIndex, shuffle);
}
