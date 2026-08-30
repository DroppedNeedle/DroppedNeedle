import { afterEach, beforeEach, expect, it } from 'vitest';
import { get, set } from 'idb-keyval';
import { PERSISTER_KEY_PREFIX, type PersistedQuery } from '@tanstack/svelte-query-persist-client';
import { clearPersistedQueryCache } from './IndexedDbPersister.svelte';
import { invalidateMusicBrainzProviderQueries } from './QueryClient';

const keyPrefix = `${PERSISTER_KEY_PREFIX}-`;

function persistedQuery(queryKey: readonly unknown[], data: unknown): PersistedQuery {
	return {
		buster: '',
		queryHash: JSON.stringify(queryKey),
		queryKey: [...queryKey],
		state: {
			data,
			dataUpdateCount: 1,
			dataUpdatedAt: Date.now(),
			error: null,
			errorUpdateCount: 0,
			errorUpdatedAt: 0,
			fetchFailureCount: 0,
			fetchFailureReason: null,
			fetchMeta: null,
			isInvalidated: false,
			status: 'success',
			fetchStatus: 'idle'
		}
	};
}

beforeEach(async () => {
	await clearPersistedQueryCache();
});

afterEach(async () => {
	await clearPersistedQueryCache();
});

it('removes only provider-bearing IndexedDB rows and retains unrelated or malformed rows', async () => {
	const providerKey = `${keyPrefix}provider-artist`;
	const editionKey = `${keyPrefix}provider-edition`;
	const profileKey = `${keyPrefix}profile`;
	const lastFmKey = `${keyPrefix}lastfm`;
	const localSearchKey = `${keyPrefix}local-search`;
	const malformedKey = `${keyPrefix}malformed`;
	const numericKey = 42;
	const unrelatedKey = 'unrelated-application-row';

	await Promise.all([
		set(providerKey, persistedQuery(['artist', 'artist-1'], { provider: 'musicbrainz' })),
		set(editionKey, persistedQuery(['albums', 'editions', 'user-a', 'release-group-1'], {})),
		set(profileKey, persistedQuery(['profile', 'u'], { display_name: 'User' })),
		set(
			lastFmKey,
			persistedQuery(['artist', 'artist-1', 'lastfm-enrichment', { artistName: 'Artist' }], {})
		),
		set(localSearchKey, persistedQuery(['search', 'user-a', 'local-artists', 'artist'], {})),
		set(malformedKey, { malformed: true }),
		set(numericKey, { malformed: true }),
		set(unrelatedKey, { unrelated: true })
	]);

	await invalidateMusicBrainzProviderQueries();

	expect(await get(providerKey)).toBeUndefined();
	expect(await get(editionKey)).toBeUndefined();
	expect(await get(profileKey)).toEqual(expect.objectContaining({ queryKey: ['profile', 'u'] }));
	expect(await get(lastFmKey)).toEqual(
		expect.objectContaining({
			queryKey: ['artist', 'artist-1', 'lastfm-enrichment', { artistName: 'Artist' }]
		})
	);
	expect(await get(localSearchKey)).toEqual(
		expect.objectContaining({ queryKey: ['search', 'user-a', 'local-artists', 'artist'] })
	);
	expect(await get(malformedKey)).toEqual({ malformed: true });
	expect(await get(numericKey)).toEqual({ malformed: true });
	expect(await get(unrelatedKey)).toEqual({ unrelated: true });
});
