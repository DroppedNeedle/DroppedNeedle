import { playerStore } from '$lib/stores/player.svelte';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import type { PlaybackMeta, QueueItem } from '$lib/player/types';
import type { RemoteTrack } from '$lib/queries/remotes/types';
import { getCoverUrl } from '$lib/utils/errorHandling';
import {
	normalizeDiscNumber,
	recordingFor,
	REMOTE_FORMAT,
	remoteStreamKey
} from '$lib/player/queueHelpers';

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
	const normalizedCoverUrl = getCoverUrl(meta.coverUrl, meta.albumId);

	const streamable = tracks.flatMap((track) => {
		const key = remoteStreamKey(track);
		return key ? [{ track, key }] : [];
	});
	if (!streamable.length) return;
	const startAt = streamable.findIndex(({ track }) => track === tracks[startIndex]);

	const items: QueueItem[] = streamable.map(({ track: t, key }) => ({
		trackSourceId: key,
		trackName: t.title,
		artistName: meta.artistName,
		trackNumber: t.track_number ?? 0,
		discNumber: normalizeDiscNumber(t.disc_number),
		albumId: meta.albumId,
		albumName: meta.albumName,
		coverUrl: normalizedCoverUrl,
		sourceType: t.source,
		artistId: meta.artistId,
		streamUrl: gatewayStreamUrl(t.source, key),
		format: REMOTE_FORMAT,
		recordingIds: recordingFor(t.source, t.recording_mbid),
		plexRatingKey: t.source === 'plex' ? t.id : undefined
	}));

	playerStore.playQueue(items, Math.max(startAt, 0), shuffle);
}
