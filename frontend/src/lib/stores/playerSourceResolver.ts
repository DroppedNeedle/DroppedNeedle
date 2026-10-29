import type { NowPlaying, QueueItem } from '$lib/player/types';

export function buildNowPlayingMetadata(item: QueueItem): NowPlaying {
	return {
		albumId: item.albumId,
		albumName: item.albumName,
		artistName: item.artistName,
		coverUrl: item.coverUrl,
		coverRemoteUrl: item.coverRemoteUrl,
		isPreview: item.isPreview,
		sourceType: item.sourceType,
		discNumber: item.discNumber,
		trackSourceId: item.trackSourceId,
		trackName: item.trackName,
		artistId: item.artistId,
		format: item.format,
		playlistTrackId: item.playlistTrackId,
		recordingMbid: item.recordingIds?.[item.sourceType]
	};
}
