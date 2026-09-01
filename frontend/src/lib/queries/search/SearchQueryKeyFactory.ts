import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';

const providerAll = (userId: string | null | undefined) => {
	const normalizedUserId = userId ?? null;
	return ['search', normalizedUserId, musicBrainzSourceKey(normalizedUserId)] as const;
};

export const SearchQueryKeyFactory = {
	all: (userId: string | null | undefined) => ['search', userId ?? null] as const,
	localArtists: (userId: string | null | undefined, query: string, limit: number) =>
		[
			...SearchQueryKeyFactory.all(userId),
			'local-artists',
			query.trim().toLowerCase(),
			limit
		] as const,
	localAlbums: (userId: string | null | undefined, query: string, limit: number) =>
		[
			...SearchQueryKeyFactory.all(userId),
			'local-albums',
			query.trim().toLowerCase(),
			limit
		] as const,
	artists: (userId: string | null | undefined, query: string, limit: number) =>
		[...providerAll(userId), 'artists', query.trim().toLowerCase(), limit] as const,
	albums: (userId: string | null | undefined, query: string, limit: number) =>
		[...providerAll(userId), 'albums', query.trim().toLowerCase(), limit] as const,
	suggestions: (userId: string | null | undefined, query: string, limit: number) =>
		[...providerAll(userId), 'suggestions', query.trim().toLowerCase(), limit] as const
};
