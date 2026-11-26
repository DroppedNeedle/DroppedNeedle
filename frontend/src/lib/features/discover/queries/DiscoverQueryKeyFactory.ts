import type { AlbumPreviewMode, ListenSource } from './types';
import { key, userScope } from '$lib/queries/keys';

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

const root = (userId: DiscoverV3UserId) => key(userScope(userId), 'discover');

export const DiscoverQueryKeyFactory = {
	feature: 'discover',
	v3: {
		root,
		home: (userId: DiscoverV3UserId) => [...root(userId), 'home'] as const,
		queueStatus: (userId: DiscoverV3UserId) => [...root(userId), 'queue-status'] as const,
		feedback: (userId: DiscoverV3UserId) => [...root(userId), 'feedback'] as const,
		tunings: (userId: DiscoverV3UserId) => [...root(userId), 'tunings'] as const,
		// The Later pile, and whether one album is on it, under the pile's prefix.
		later: (userId: DiscoverV3UserId) => [...root(userId), 'later'] as const,
		laterAlbum: (userId: DiscoverV3UserId, releaseGroupMbid: string) =>
			[...root(userId), 'later', 'album', releaseGroupMbid] as const,
		// The week's set, and "dig further" past it, under the week's prefix.
		week: (userId: DiscoverV3UserId) => [...root(userId), 'week'] as const,
		weekMore: (userId: DiscoverV3UserId) => [...root(userId), 'week', 'more'] as const,
		// The caller's new releases from their artists.
		newReleases: (userId: DiscoverV3UserId) => [...root(userId), 'new-releases'] as const,
		// The caller's past weeks, every filter and search under one prefix,
		// and the albums they kept from them.
		pastWeeks: (userId: DiscoverV3UserId) => [...root(userId), 'weeks'] as const,
		pastWeeksList: (userId: DiscoverV3UserId, state: string | null, search: string) =>
			[...root(userId), 'weeks', state ?? 'all', search] as const,
		found: (userId: DiscoverV3UserId) => [...root(userId), 'found'] as const,
		// The caller's set day.
		prefs: (userId: DiscoverV3UserId) => [...root(userId), 'prefs'] as const,
		// What "Why this?" opens for one pick, and every pick's, as a prefix.
		pickEvidence: (userId: DiscoverV3UserId, releaseGroupMbid: string) =>
			[...root(userId), 'pick-evidence', releaseGroupMbid] as const,
		allPickEvidence: (userId: DiscoverV3UserId) => [...root(userId), 'pick-evidence'] as const,
		batches: (userId: DiscoverV3UserId) => [...root(userId), 'batches'] as const,
		batch: (userId: DiscoverV3UserId, batchId: string) =>
			[...root(userId), 'batch', batchId] as const,
		radio: (
			userId: DiscoverV3UserId,
			seedType: string,
			seedId: string,
			params: DiscoverV3RadioParams
		) => [...root(userId), 'radio', seedType, seedId, params] as const,
		// Every radio shelf, plan and suggestion list, as prefixes.
		radios: (userId: DiscoverV3UserId) => [...root(userId), 'radio'] as const,
		radioPlans: (userId: DiscoverV3UserId) => [...root(userId), 'radio-plan'] as const,
		allPlaylistSuggestions: (userId: DiscoverV3UserId) =>
			[...root(userId), 'playlist-suggestions'] as const,
		radioPlan: (userId: DiscoverV3UserId, fingerprint: DiscoverV3RadioPlanFingerprint) =>
			[...root(userId), 'radio-plan', fingerprint] as const,
		playlistSuggestions: (userId: DiscoverV3UserId, playlistId: string, count: number) =>
			[...root(userId), 'playlist-suggestions', playlistId, count] as const,
		albumPreview: (
			userId: DiscoverV3UserId,
			artist: string,
			album: string,
			mode: AlbumPreviewMode
		) => [...root(userId), 'album-preview', artist, album, mode] as const,
		trackPreview: (userId: DiscoverV3UserId, artist: string, track: string) =>
			[...root(userId), 'track-preview', artist, track] as const,
		youtubeTrackSearch: (userId: DiscoverV3UserId, artist: string, track: string) =>
			[...root(userId), 'youtube-track-search', artist, track] as const
	}
};
