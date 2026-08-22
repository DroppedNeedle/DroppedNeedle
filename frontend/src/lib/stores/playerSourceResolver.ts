import { getApiUrl } from '$lib/api/api-utils';
import { API } from '$lib/constants';
import type { NowPlaying, QueueItem, SourceType } from '$lib/player/types';

export function resolveSourceUrl(item: QueueItem): string | undefined {
	switch (item.sourceType) {
		case 'youtube':
			return item.streamUrl;
		case 'local':
			return getApiUrl(item.streamUrl ?? API.stream.local(item.trackSourceId));
		case 'navidrome':
			return getApiUrl(item.streamUrl ?? API.stream.navidrome(item.trackSourceId));
		case 'jellyfin':
			return getApiUrl(API.stream.jellyfin(item.trackSourceId));
		case 'plex':
			return getApiUrl(item.streamUrl ?? API.stream.plex(item.trackSourceId));
	}
}

export function buildPrefetchUrl(item: QueueItem): string | null {
	switch (item.sourceType) {
		case 'youtube':
			return null;
		case 'jellyfin':
			return API.stream.jellyfin(item.trackSourceId);
		case 'navidrome':
			return API.stream.navidrome(item.trackSourceId);
		case 'plex':
			return API.stream.plex(item.trackSourceId);
		case 'local':
			return API.stream.local(item.trackSourceId);
		default:
			return item.streamUrl ?? null;
	}
}

export function buildStreamUrlForSource(
	sourceType: SourceType,
	trackSourceId: string
): string | undefined {
	switch (sourceType) {
		case 'local':
			return API.stream.local(trackSourceId);
		case 'navidrome':
			return API.stream.navidrome(trackSourceId);
		case 'jellyfin':
			return API.stream.jellyfin(trackSourceId);
		case 'plex':
			return API.stream.plex(trackSourceId);
		default:
			return undefined;
	}
}

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
		streamUrl: item.streamUrl,
		format: item.format,
		playlistTrackId: item.playlistTrackId
	};
}
