import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createInfiniteQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: 'keepPreviousData'
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	setQueryDataWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import {
	cacheCanonicalLibraryAlbumDetailV3,
	getLibraryAlbumDetailV3QueryOptions,
	getLibraryAlbumsV3QueryOptions,
	getLibraryArtistsV3InfiniteQuery,
	getLibraryEditionPinV3QueryOptions,
	getLibraryGenresV3Query,
	getLibraryLyricsV3QueryOptions,
	getLibraryRecentlyAddedV3Query,
	getLibraryReviewsV3QueryOptions,
	getLibraryRootsV3Query,
	getLibraryScanRunsV3Query,
	getLibraryStatsV3QueryOptions,
	getLibraryTracksV3QueryOptions
} from './LibraryV3Queries.svelte';
import { LibraryV3Api } from './LibraryV3Api';

beforeEach(() => vi.clearAllMocks());

async function callQueryFn(
	options: { queryFn?: unknown },
	ctx: { signal?: AbortSignal }
): Promise<unknown> {
	const queryFn = options.queryFn as (ctx: { signal?: AbortSignal }) => Promise<unknown>;
	return queryFn(ctx);
}

describe('LibraryV3Queries', () => {
	it('fetches album pages from the v3 catalog with paging params on a user-scoped key', async () => {
		const params = { limit: 50, offset: 0, sort: 'name', order: 'asc' } as const;
		const options = getLibraryAlbumsV3QueryOptions('user-1', params);
		expect(options.queryKey).toEqual(LibraryQueryKeyFactory.v3.albums('user-1', params));

		const signal = new AbortController().signal;
		await callQueryFn(options, { signal });
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/library/albums?limit=50&offset=0&sort=name&order=asc',
			{ signal }
		);
	});

	it('passes album filters through and disables the query without a user', () => {
		const options = getLibraryAlbumsV3QueryOptions('user-1', {
			limit: 50,
			offset: 0,
			sort: 'year',
			order: 'desc',
			q: 'Blue',
			decade: 1990
		});
		expect(options.queryKey).toContainEqual(expect.objectContaining({ q: 'Blue' }));
		expect(
			getLibraryAlbumsV3QueryOptions(undefined, { limit: 1, offset: 0, sort: 'name', order: 'asc' })
				.enabled
		).toBe(false);
	});

	it('sends the artist sort and format filter to the catalog', async () => {
		const options = getLibraryAlbumsV3QueryOptions('user-1', {
			limit: 50,
			offset: 50,
			sort: 'artist',
			order: 'asc',
			format: 'flac'
		});
		const signal = new AbortController().signal;
		await callQueryFn(options, { signal });
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/library/albums?limit=50&offset=50&sort=artist&order=asc&format=flac',
			{ signal }
		);
	});

	it('pages artists by offset until the total is loaded', async () => {
		const query = getLibraryArtistsV3InfiniteQuery(() => ({
			sortBy: 'name',
			sortOrder: 'asc',
			q: '',
			scope: 'all'
		})) as unknown as {
			queryFn: (args: { pageParam?: number }) => Promise<unknown>;
			getNextPageParam: (
				last: { items: unknown[]; total: number },
				all: { items: unknown[] }[]
			) => number | undefined;
		};
		expect(query.getNextPageParam({ items: [1], total: 3 }, [{ items: [1] }])).toBe(1);
		expect(
			query.getNextPageParam({ items: [1, 2, 3], total: 3 }, [{ items: [1, 2, 3] }])
		).toBeUndefined();
		await query.queryFn({ pageParam: 48 });
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			expect.stringContaining('/api/v3/library/artists?'),
			expect.anything()
		);
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			expect.stringContaining('offset=48'),
			expect.anything()
		);
	});

	it('gates detail queries on user and id', () => {
		expect(getLibraryAlbumDetailV3QueryOptions('user-1', 'album-1').enabled).toBe(true);
		expect(getLibraryAlbumDetailV3QueryOptions('user-1', '').enabled).toBe(false);
		expect(getLibraryAlbumDetailV3QueryOptions(undefined, 'album-1').enabled).toBe(false);
		expect(LibraryV3Api.albumDetail('a/b')).toBe('/api/v3/library/albums/a%2Fb');
	});

	it('caches canonical album detail through the persister-safe helper', async () => {
		const album = { id: 'album-1', title: 'Blue' };
		await cacheCanonicalLibraryAlbumDetailV3('user-1', album as never);
		expect(setQueryDataWithPersister).toHaveBeenCalledWith(
			LibraryQueryKeyFactory.v3.albumDetail('user-1', 'album-1'),
			album
		);
	});

	it('keeps lyrics cached for the long lyrics window', () => {
		expect(getLibraryLyricsV3QueryOptions('user-1', 'track-1').staleTime).toBe(CACHE_TTL.LYRICS);
	});

	it('hits the v3 review, pin, runs, roots, stats, tracks, and genre endpoints', () => {
		expect(LibraryV3Api.reviews('album-1')).toBe('/api/v3/library/reviews?album_id=album-1');
		expect(LibraryV3Api.editionPin('album-1')).toBe('/api/v3/library/albums/album-1/edition-pin');
		expect(LibraryV3Api.scanRuns()).toBe('/api/v3/library/scan/runs');
		expect(LibraryV3Api.roots()).toBe('/api/v3/library/roots');
		expect(getLibraryStatsV3QueryOptions('user-1').queryKey).toEqual(
			LibraryQueryKeyFactory.v3.stats('user-1')
		);
		expect(
			getLibraryTracksV3QueryOptions('user-1', {
				limit: 10,
				offset: 0,
				sort: 'title',
				order: 'asc'
			}).queryKey[3]
		).toBe('tracks');
		expect(getLibraryReviewsV3QueryOptions('user-1', 'album-1').queryKey).toEqual(
			LibraryQueryKeyFactory.v3.reviews('user-1', 'album-1')
		);
		expect(getLibraryEditionPinV3QueryOptions('user-1', 'album-1').queryKey).toEqual(
			LibraryQueryKeyFactory.v3.editionPin('user-1', 'album-1')
		);
	});

	it('exposes stats, genres, recently-added, runs, and roots hooks', () => {
		expect(getLibraryGenresV3Query).toBeTypeOf('function');
		expect(getLibraryRecentlyAddedV3Query).toBeTypeOf('function');
		expect(getLibraryScanRunsV3Query).toBeTypeOf('function');
		expect(getLibraryRootsV3Query).toBeTypeOf('function');
	});
});
