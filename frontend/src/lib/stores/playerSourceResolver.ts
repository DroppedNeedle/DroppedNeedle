import { getApiUrl } from '$lib/api/api-utils';
import { gatewayStreamUrl } from '$lib/player/playbackGateway';
import type { NowPlaying, QueueItem, SourceType } from '$lib/player/types';

// Raw URL computation only - base-path composition happens once at each
// exported boundary below.
function rawSourceUrl(item: QueueItem): string | undefined {
	switch (item.sourceType) {
		case 'youtube':
			return item.streamUrl;
		case 'local':
			return item.streamUrl ?? gatewayStreamUrl('local', item.trackSourceId);
		case 'navidrome':
			return item.streamUrl ?? gatewayStreamUrl('navidrome', item.trackSourceId);
		case 'jellyfin':
			return gatewayStreamUrl('jellyfin', item.trackSourceId);
		case 'plex':
			return item.streamUrl ?? gatewayStreamUrl('plex', item.trackSourceId);
	}
}

function rawPrefetchUrl(item: QueueItem): string | null {
	switch (item.sourceType) {
		case 'youtube':
			return null;
		case 'jellyfin':
			return gatewayStreamUrl('jellyfin', item.trackSourceId);
		case 'navidrome':
			return gatewayStreamUrl('navidrome', item.trackSourceId);
		case 'plex':
			return gatewayStreamUrl('plex', item.trackSourceId);
		case 'local':
			return gatewayStreamUrl('local', item.trackSourceId);
		default:
			return item.streamUrl ?? null;
	}
}

export function resolveSourceUrl(item: QueueItem): string | undefined {
	const url = rawSourceUrl(item);
	// YouTube stream URLs stay byte-for-byte; everything else gets the base.
	if (item.sourceType === 'youtube' || url === undefined) return url;
	return getApiUrl(url);
}

export function buildPrefetchUrl(item: QueueItem): string | null {
	const url = rawPrefetchUrl(item);
	if (url === null) return null;
	return getApiUrl(url);
}

function rawStreamUrlForSource(sourceType: SourceType, trackSourceId: string): string | undefined {
	switch (sourceType) {
		case 'local':
			return gatewayStreamUrl('local', trackSourceId);
		case 'navidrome':
			return gatewayStreamUrl('navidrome', trackSourceId);
		case 'jellyfin':
			return gatewayStreamUrl('jellyfin', trackSourceId);
		case 'plex':
			return gatewayStreamUrl('plex', trackSourceId);
		default:
			return undefined;
	}
}

export function buildStreamUrlForSource(
	sourceType: SourceType,
	trackSourceId: string
): string | undefined {
	const url = rawStreamUrlForSource(sourceType, trackSourceId);
	if (url === undefined) return undefined;
	return getApiUrl(url);
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
