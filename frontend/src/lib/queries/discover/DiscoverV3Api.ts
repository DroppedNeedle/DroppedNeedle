import { v3 } from '$lib/api/v3/endpoint';

// /api/v3 discover URLs, built through the typed registry: every template
// is a literal the contract-coverage gate verifies against the generated
// spec. Nothing outside this feature imports them.
export const DiscoverV3Api = {
	home: () => v3('/api/v3/discover'),
	refresh: () => v3('/api/v3/discover/refresh'),
	activity: () => v3('/api/v3/discover/activity'),
	queue: (count: number | null) =>
		count === null
			? v3('/api/v3/discover/queue')
			: v3('/api/v3/discover/queue', { query: { count } }),
	queueStatus: () => v3('/api/v3/discover/queue/status'),
	queueGenerate: () => v3('/api/v3/discover/queue/generate'),
	queueEnrich: (releaseGroupMbid: string) =>
		v3('/api/v3/discover/queue/enrich/{release_group_mbid}', {
			path: { release_group_mbid: releaseGroupMbid }
		}),
	queuePreview: (releaseGroupMbid: string) =>
		v3('/api/v3/discover/queue/preview/{release_group_mbid}', {
			path: { release_group_mbid: releaseGroupMbid }
		}),
	queueIgnore: () => v3('/api/v3/discover/queue/ignore'),
	ignored: () => v3('/api/v3/discover/queue/ignored'),
	queueValidate: () => v3('/api/v3/discover/queue/validate'),
	batches: () => v3('/api/v3/discover/batches'),
	batch: (batchId: string) =>
		v3('/api/v3/discover/batches/{batch_id}', { path: { batch_id: batchId } }),
	batchRemove: (batchId: string, removeAlbums: boolean) =>
		v3('/api/v3/discover/batches/{batch_id}', {
			path: { batch_id: batchId },
			query: { remove_albums: removeAlbums }
		}),
	radio: () => v3('/api/v3/discover/radio'),
	radioPlan: () => v3('/api/v3/discover/radio/plan'),
	playlistSuggestions: () => v3('/api/v3/discover/playlist-suggestions'),
	albumPreview: (artist: string, album: string, count: number | null) =>
		v3('/api/v3/discover/album-preview', {
			query: count === null ? { artist, album } : { artist, album, count }
		}),
	trackPreview: (artist: string, track: string) =>
		v3('/api/v3/discover/track-preview', { query: { artist, track } }),
	youtubeSearch: (artist: string, album: string) =>
		v3('/api/v3/discover/queue/youtube-search', { query: { artist, album } }),
	youtubeTrackSearch: (artist: string, track: string) =>
		v3('/api/v3/discover/queue/youtube-track-search', { query: { artist, track } }),
	youtubeQuota: () => v3('/api/v3/discover/queue/youtube-quota'),
	youtubeCacheCheck: () => v3('/api/v3/discover/queue/youtube-cache-check')
};
