import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';
import { userIdSegment } from '../userKeySegment';

// Every personalized discover key carries a userId and provider source identity.
export type DiscoverV3UserId = string | null | undefined;

export interface DiscoverV3RadioParams {
	count?: number;
	source?: string | null;
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

const v3SourceKey = (userId: DiscoverV3UserId) => {
	const normalizedUserId = userIdSegment(userId);
	return {
		normalizedUserId,
		sourceKey: musicBrainzSourceKey(normalizedUserId)
	};
};

export const DiscoverQueryKeyFactory = {
	prefix: ['discover'] as const,
	// v3 discover keys. Provider-backed shelves embed the provider source
	// identity and join the source-switch sweep; user-owned records (batches,
	// ignore ledger) and provider-independent lookups (YouTube) carry only the
	// userId.
	v3: {
		root: (userId: DiscoverV3UserId) =>
			[...DiscoverQueryKeyFactory.prefix, 'v3', userIdSegment(userId)] as const,
		home: (userId: DiscoverV3UserId) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [...DiscoverQueryKeyFactory.v3.root(normalizedUserId), sourceKey, 'home'] as const;
		},
		queue: (userId: DiscoverV3UserId, count: number | null) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'queue',
				count
			] as const;
		},
		queueStatus: (userId: DiscoverV3UserId) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'queue-status'
			] as const;
		},
		queueEnrich: (userId: DiscoverV3UserId, releaseGroupMbid: string) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'queue-enrich',
				releaseGroupMbid
			] as const;
		},
		ignored: (userId: DiscoverV3UserId) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'ignored'] as const,
		batches: (userId: DiscoverV3UserId) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'batches'] as const,
		batch: (userId: DiscoverV3UserId, batchId: string) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'batch', batchId] as const,
		radio: (
			userId: DiscoverV3UserId,
			seedType: string,
			seedId: string,
			params: DiscoverV3RadioParams
		) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'radio',
				seedType,
				seedId,
				params
			] as const;
		},
		radioPlan: (userId: DiscoverV3UserId, fingerprint: DiscoverV3RadioPlanFingerprint) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'radio-plan',
				fingerprint
			] as const;
		},
		playlistSuggestions: (userId: DiscoverV3UserId, playlistId: string, count: number) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'playlist-suggestions',
				playlistId,
				count
			] as const;
		},
		albumPreview: (
			userId: DiscoverV3UserId,
			artist: string,
			album: string,
			count: number | null
		) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'album-preview',
				artist,
				album,
				count
			] as const;
		},
		trackPreview: (userId: DiscoverV3UserId, artist: string, track: string) => {
			const { normalizedUserId, sourceKey } = v3SourceKey(userId);
			return [
				...DiscoverQueryKeyFactory.v3.root(normalizedUserId),
				sourceKey,
				'track-preview',
				artist,
				track
			] as const;
		},
		youtubeSearch: (userId: DiscoverV3UserId, artist: string, album: string) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'youtube-search', artist, album] as const,
		youtubeTrackSearch: (userId: DiscoverV3UserId, artist: string, track: string) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'youtube-track-search', artist, track] as const,
		youtubeQuota: (userId: DiscoverV3UserId) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'youtube-quota'] as const,
		youtubeCacheCheck: (userId: DiscoverV3UserId, items: DiscoverV3CacheCheckItem[]) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'youtube-cache-check', items] as const,
		queueValidate: (userId: DiscoverV3UserId, releaseGroupMbids: string[]) =>
			[...DiscoverQueryKeyFactory.v3.root(userId), 'queue-validate', releaseGroupMbids] as const
	},
	discover: (userId: string | null | undefined) => {
		const normalizedUserId = userIdSegment(userId);
		return [
			...DiscoverQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId)
		] as const;
	},
	radio: (userId: string | null | undefined, seedType: string, seedId: string) => {
		const normalizedUserId = userIdSegment(userId);
		return [
			...DiscoverQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId),
			'radio',
			seedType,
			seedId
		] as const;
	},
	playlistSuggestions: (userId: string | null | undefined, playlistId: string) => {
		const normalizedUserId = userIdSegment(userId);
		return [
			...DiscoverQueryKeyFactory.prefix,
			normalizedUserId,
			musicBrainzSourceKey(normalizedUserId),
			'playlist-suggestions',
			playlistId
		] as const;
	}
};
