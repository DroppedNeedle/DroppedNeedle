import { beforeEach, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	removeQueries: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('@tanstack/svelte-query-persist-client', () => ({
	PERSISTER_KEY_PREFIX: 'tanstack-query',
	experimental_createQueryPersister: vi.fn(() => ({
		persisterFn: vi.fn(),
		persistQueryByKey: vi.fn().mockResolvedValue(undefined),
		removeQueries: h.removeQueries
	}))
}));

const idb = vi.hoisted(() => ({
	clear: vi.fn().mockResolvedValue(undefined),
	del: vi.fn().mockResolvedValue(undefined),
	entries: vi.fn().mockResolvedValue([]),
	get: vi.fn().mockResolvedValue(undefined),
	set: vi.fn().mockResolvedValue(undefined)
}));

vi.mock('idb-keyval', () => idb);

import {
	invalidateMusicBrainzProviderQueries,
	invalidateQueriesWithPersister,
	queryClient,
	resetQueryCacheForUserSwitch,
	setQueryDataWithPersister
} from './QueryClient';

beforeEach(() => {
	vi.clearAllMocks();
	h.removeQueries.mockResolvedValue(undefined);
	queryClient.clear();
});

it('clears both memory and persisted data before an account switch', async () => {
	const oldUserKey = ['me', 'scrobble-preferences', 'user-a'] as const;
	await setQueryDataWithPersister(oldUserKey, {
		primary_music_source: 'lastfm'
	});
	expect(queryClient.getQueryData(oldUserKey)).toEqual({ primary_music_source: 'lastfm' });

	await resetQueryCacheForUserSwitch();

	expect(queryClient.getQueryData(oldUserKey)).toBeUndefined();
	expect(idb.clear).toHaveBeenCalledOnce();
});

it('keeps persisted rows on invalidation by default', async () => {
	await invalidateQueriesWithPersister({ queryKey: ['library'] });

	expect(h.removeQueries).not.toHaveBeenCalled();
});

it('destroys persisted rows only when removePersisted is opted into', async () => {
	await invalidateQueriesWithPersister({ queryKey: ['library'] }, undefined, {
		removePersisted: true
	});

	expect(h.removeQueries).toHaveBeenCalledOnce();
	expect(h.removeQueries).toHaveBeenCalledWith({ queryKey: ['library'] });
});

it('sweeps only correctness-bearing provider queries exactly once', async () => {
	const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries');
	idb.entries.mockResolvedValueOnce([
		[
			'tanstack-query-provider',
			{
				buster: '',
				queryHash: 'provider',
				queryKey: ['artist', 'artist-1'],
				state: {}
			}
		],
		[
			'tanstack-query-profile',
			{
				buster: '',
				queryHash: 'profile',
				queryKey: ['profile', 'user-a'],
				state: {}
			}
		]
	]);

	await invalidateMusicBrainzProviderQueries();

	expect(h.removeQueries).not.toHaveBeenCalled();
	expect(idb.entries).toHaveBeenCalledOnce();
	expect(idb.del).toHaveBeenCalledWith('tanstack-query-provider');
	expect(invalidateSpy).toHaveBeenCalledOnce();
	const filters = invalidateSpy.mock.calls[0][0] as {
		predicate?: (query: { queryKey: readonly unknown[] }) => boolean;
	};
	const predicate = filters.predicate;
	expect(predicate).toBeDefined();
	if (!predicate) throw new Error('provider invalidation predicate missing');

	expect(predicate({ queryKey: ['artist', 'artist-1'] })).toBe(true);
	expect(predicate({ queryKey: ['artist', 'artist-1', 'extended'] })).toBe(true);
	expect(predicate({ queryKey: ['artist', 'artist-1', 'releases'] })).toBe(true);
	expect(predicate({ queryKey: ['artist', 'artist-1', 'lastfm-enrichment'] })).toBe(false);
	expect(predicate({ queryKey: ['artist', 'artist-1', 'top-albums'] })).toBe(false);
	expect(predicate({ queryKey: ['artist', 'artist-1', 'similar-artists'] })).toBe(false);
	expect(predicate({ queryKey: ['albums', 'editions', 'user-a', 'rg-1'] })).toBe(true);
	expect(predicate({ queryKey: ['albums', 'purchase-options', 'v2', 'rg-1'] })).toBe(false);
	expect(predicate({ queryKey: ['search', 'user-a', 'artists', 'radiohead', 10] })).toBe(true);
	expect(predicate({ queryKey: ['search', 'user-a', 'albums', 'radiohead', 10] })).toBe(true);
	expect(predicate({ queryKey: ['search', 'user-a', 'suggestions', 'radiohead', 10] })).toBe(true);
	expect(predicate({ queryKey: ['search', 'user-a', 'local-artists', 'radiohead', 10] })).toBe(
		false
	);
	// The mixed discover home response intentionally remains a whole-payload correctness boundary.
	expect(predicate({ queryKey: ['discover', 'user-a'] })).toBe(true);
	expect(predicate({ queryKey: ['discover', 'user-a', 'radio', 'artist', 'artist-1'] })).toBe(true);
	expect(
		predicate({ queryKey: ['discover', 'user-a', 'playlist-suggestions', 'playlist-1'] })
	).toBe(true);
	expect(predicate({ queryKey: ['discover', 'user-a', 'batches'] })).toBe(false);
	expect(predicate({ queryKey: ['discover', 'user-a', 'integrations'] })).toBe(false);
	expect(
		predicate({
			queryKey: [
				'artist',
				{ user_id: 'user-a', source_mode: 'official', source_id: 's1', generation: 1 },
				'artist-1'
			]
		})
	).toBe(true);
	expect(
		predicate({
			queryKey: [
				'search',
				'user-a',
				{ user_id: 'user-a', source_mode: 'official', source_id: 's1', generation: 1 },
				'artists'
			]
		})
	).toBe(true);
	expect(
		predicate({
			queryKey: [
				'home',
				'user-a',
				{ user_id: 'user-a', source_mode: 'official', source_id: 's1', generation: 1 }
			]
		})
	).toBe(true);

	invalidateSpy.mockRestore();
});

it('still invalidates active queries and rethrows a persisted removal failure', async () => {
	const persistedFailure = new Error('persisted cache unavailable');
	const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries');
	h.removeQueries.mockRejectedValueOnce(persistedFailure);

	await expect(
		invalidateQueriesWithPersister({ queryKey: ['albums'] }, undefined, {
			removePersisted: true
		})
	).rejects.toBe(persistedFailure);

	expect(invalidateSpy).toHaveBeenCalledOnce();
	invalidateSpy.mockRestore();
});

it('keeps selective cleanup retry-safe when a persisted provider row cannot be deleted', async () => {
	const persistedFailure = new Error('provider row unavailable');
	const invalidateSpy = vi.spyOn(queryClient, 'invalidateQueries');
	idb.entries.mockResolvedValueOnce([
		[
			'tanstack-query-provider',
			{
				buster: '',
				queryHash: 'provider',
				queryKey: ['artist', 'artist-1'],
				state: {}
			}
		]
	]);
	idb.del.mockRejectedValueOnce(persistedFailure);

	await expect(invalidateMusicBrainzProviderQueries()).rejects.toBe(persistedFailure);
	expect(invalidateSpy).toHaveBeenCalledOnce();
	invalidateSpy.mockRestore();
});
