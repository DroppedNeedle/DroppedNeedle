import { describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	// LibraryQueries pulls in ../QueryClient, which news up the real client at import
	QueryClient: class {}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { CACHE_TTL } from '$lib/constants';

import { getHomeQuery, getHomeQueryOptions } from '../HomeQuery.svelte';
import { HomeQueryKeyFactory } from '../HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

import {
	getLibraryAlbumCopiesQuery,
	getLibraryAlbumCopiesQueryOptions,
	getLibraryAlbumDetailQuery,
	getLibraryAlbumDetailQueryOptions,
	getLibraryAlbumStatusQueryOptions,
	getLibraryArtistDetailQuery,
	getLibraryArtistDetailQueryOptions
} from './LibraryQueries.svelte';

// B7: the detail queries were split into queryOptions factories (prefetch surface) plus
// thin createQuery wrappers. The wrappers must keep producing exactly the option shape
// the old inline literals produced - same keys, same staleTime, same queryFn - with the
// enabled gate layered on top.
describe('library/home detail queryOptions factories (B7)', () => {
	it('album factory: same key/staleTime/queryFn shape as the former inline options', () => {
		const prefetchOpts = getLibraryAlbumDetailQueryOptions('alb-1');
		expect(prefetchOpts.queryKey).toEqual(LibraryQueryKeyFactory.albumDetail('alb-1'));
		expect(prefetchOpts.staleTime).toBe(CACHE_TTL.LIBRARY_NATIVE);
		expect(typeof prefetchOpts.queryFn).toBe('function');

		const wrapped = getLibraryAlbumDetailQuery(() => 'alb-1') as unknown as Record<string, unknown>;
		expect(wrapped.queryKey).toEqual(prefetchOpts.queryKey);
		expect(wrapped.staleTime).toBe(prefetchOpts.staleTime);
		expect(typeof wrapped.queryFn).toBe('function');
		expect(wrapped.enabled).toBe(true);
		expect(
			(getLibraryAlbumDetailQuery(() => '') as unknown as Record<string, unknown>).enabled
		).toBe(false);
	});

	it('artist factory: same key/staleTime/queryFn shape as the former inline options', () => {
		const prefetchOpts = getLibraryArtistDetailQueryOptions('art-1');
		expect(prefetchOpts.queryKey).toEqual(LibraryQueryKeyFactory.artistDetail('art-1'));
		const wrapped = getLibraryArtistDetailQuery(() => 'art-1') as unknown as Record<
			string,
			unknown
		>;
		expect(wrapped.queryKey).toEqual(prefetchOpts.queryKey);
		expect(wrapped.staleTime).toBe(prefetchOpts.staleTime);
		expect(typeof wrapped.queryFn).toBe('function');
		expect(wrapped.enabled).toBe(true);
		expect(
			(getLibraryArtistDetailQuery(() => '') as unknown as Record<string, unknown>).enabled
		).toBe(false);
	});

	it('home factory: same key/staleTime shape and the thin wrapper keeps the refreshing poll', () => {
		const prefetchOpts = getHomeQueryOptions('user-1');
		expect(prefetchOpts.queryKey).toEqual(HomeQueryKeyFactory.home('user-1'));
		expect(prefetchOpts.staleTime).toBe(CACHE_TTL.HOME);
		expect(typeof prefetchOpts.queryFn).toBe('function');

		const wrapped = getHomeQuery() as unknown as Record<string, unknown>;
		expect(wrapped.queryKey).toEqual(HomeQueryKeyFactory.home('user-1'));
		expect(wrapped.staleTime).toBe(CACHE_TTL.HOME);
		expect(typeof wrapped.queryFn).toBe('function');
		expect(typeof wrapped.refetchInterval).toBe('function');
		const interval = wrapped.refetchInterval as (q: {
			state: { data?: { refreshing?: boolean } };
		}) => number | false;
		expect(interval({ state: { data: { refreshing: true } } })).toBe(10_000);
		expect(interval({ state: { data: { refreshing: false } } })).toBe(false);
	});
});

describe('ST7 W1 album prefetch surface', () => {
	it('copies factory: key byte-equal to the mounted query, wrapper keeps enabled gate', () => {
		const prefetchOpts = getLibraryAlbumCopiesQueryOptions('alb-1');
		expect(prefetchOpts.queryKey).toEqual(LibraryQueryKeyFactory.albumCopies('alb-1'));
		expect(prefetchOpts.queryKey).toEqual(['library', 'album-copies', 'alb-1']);
		expect(prefetchOpts.staleTime).toBe(CACHE_TTL.LIBRARY_NATIVE);
		expect(typeof prefetchOpts.queryFn).toBe('function');

		const wrapped = getLibraryAlbumCopiesQuery(() => 'alb-1') as unknown as Record<string, unknown>;
		expect(wrapped.queryKey).toEqual(prefetchOpts.queryKey);
		expect(wrapped.staleTime).toBe(prefetchOpts.staleTime);
		expect(wrapped.enabled).toBe(true);
		expect(
			(getLibraryAlbumCopiesQuery(() => '') as unknown as Record<string, unknown>).enabled
		).toBe(false);
	});

	it('status factory stays byte-equal to the album status key', () => {
		const opts = getLibraryAlbumStatusQueryOptions('alb-1');
		expect(opts.queryKey).toEqual(LibraryQueryKeyFactory.album('alb-1'));
		expect(opts.staleTime).toBe(CACHE_TTL.LIBRARY_NATIVE);
	});
});
