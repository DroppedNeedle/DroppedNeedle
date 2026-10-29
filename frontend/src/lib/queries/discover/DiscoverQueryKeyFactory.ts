import type { ListenSource } from './types';
import { key, userScope } from '../keys';

// Every Discover key is the caller's.
export type DiscoverV3UserId = string | null | undefined;

export interface DiscoverV3RadioParams {
	count?: number;
	source?: ListenSource | null;
}

export interface DiscoverV3RadioPlanFingerprint {
	mode: string;
	seedType: string;
	seedId: string | null;
}

export interface DiscoverV3CacheCheckItem {
	artist: string;
	track: string;
}

const root = (userId: DiscoverV3UserId) => key(userScope(userId), 'discover');

export const DiscoverQueryKeyFactory = {
	feature: 'discover',
	v3: {
		root,
		home: (userId: DiscoverV3UserId) => [...root(userId), 'home'] as const,
		queue: (userId: DiscoverV3UserId, count: number | null) =>
			[...root(userId), 'queue', count] as const,
		queueStatus: (userId: DiscoverV3UserId) => [...root(userId), 'queue-status'] as const,
		// Where the person is in their deck.
		savedDeck: (userId: DiscoverV3UserId) => [...root(userId), 'saved-deck'] as const,
		queueEnrich: (userId: DiscoverV3UserId, releaseGroupMbid: string) =>
			[...root(userId), 'queue-enrich', releaseGroupMbid] as const,
		ignored: (userId: DiscoverV3UserId) => [...root(userId), 'ignored'] as const,
		batches: (userId: DiscoverV3UserId) => [...root(userId), 'batches'] as const,
		batch: (userId: DiscoverV3UserId, batchId: string) =>
			[...root(userId), 'batch', batchId] as const,
		radio: (
			userId: DiscoverV3UserId,
			seedType: string,
			seedId: string,
			params: DiscoverV3RadioParams
		) => [...root(userId), 'radio', seedType, seedId, params] as const,
		radioPlan: (userId: DiscoverV3UserId, fingerprint: DiscoverV3RadioPlanFingerprint) =>
			[...root(userId), 'radio-plan', fingerprint] as const,
		playlistSuggestions: (userId: DiscoverV3UserId, playlistId: string, count: number) =>
			[...root(userId), 'playlist-suggestions', playlistId, count] as const,
		albumPreview: (userId: DiscoverV3UserId, artist: string, album: string, count: number | null) =>
			[...root(userId), 'album-preview', artist, album, count] as const,
		trackPreview: (userId: DiscoverV3UserId, artist: string, track: string) =>
			[...root(userId), 'track-preview', artist, track] as const,
		youtubeSearch: (userId: DiscoverV3UserId, artist: string, album: string) =>
			[...root(userId), 'youtube-search', artist, album] as const,
		youtubeTrackSearch: (userId: DiscoverV3UserId, artist: string, track: string) =>
			[...root(userId), 'youtube-track-search', artist, track] as const,
		youtubeQuota: (userId: DiscoverV3UserId) => [...root(userId), 'youtube-quota'] as const,
		youtubeCacheCheck: (userId: DiscoverV3UserId, items: DiscoverV3CacheCheckItem[]) =>
			[...root(userId), 'youtube-cache-check', items] as const,
		queueValidate: (userId: DiscoverV3UserId, releaseGroupMbids: string[]) =>
			[...root(userId), 'queue-validate', releaseGroupMbids] as const
	}
};
