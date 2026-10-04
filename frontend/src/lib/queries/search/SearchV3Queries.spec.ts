import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn() } } }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { SearchQueryKeyFactory } from './SearchQueryKeyFactory';
import {
	ENRICH_DEGRADED_STALE_TIME_MS,
	type EnrichmentResponseV3,
	type SearchResponseV3,
	getSearchBucketV3QueryOptions,
	getSearchEnrichBatchV3QueryOptions,
	getSearchSuggestionsV3Query,
	getUnifiedSearchV3QueryOptions,
	successfulEnrichStaleTime,
	successfulUnifiedSearchV3StaleTime
} from './SearchV3Queries.svelte';
import { SearchV3Api } from './SearchV3Api';

beforeEach(() => vi.clearAllMocks());

async function callQueryFn(
	options: { queryFn?: unknown },
	ctx: { signal?: AbortSignal }
): Promise<unknown> {
	const queryFn = options.queryFn as (ctx: { signal?: AbortSignal }) => Promise<unknown>;
	return queryFn(ctx);
}

describe('SearchV3Queries', () => {
	it('runs unified search with per-bucket limits and an optional bucket filter', async () => {
		const options = getUnifiedSearchV3QueryOptions('user-1', 'abba', {
			artists: 6,
			albums: 6,
			tracks: 6
		});
		expect(options.queryKey).toEqual(
			SearchQueryKeyFactory.v3.unified('user-1', 'abba', { artists: 6, albums: 6, tracks: 6 }, null)
		);
		const signal = new AbortController().signal;
		await callQueryFn(options, { signal });
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/search?q=abba&limit_artists=6&limit_albums=6&limit_tracks=6',
			{ signal }
		);
		expect(SearchV3Api.unified('x', { artists: 1, albums: 1, tracks: 1 }, ['artists'])).toBe(
			'/api/v3/search?q=x&limit_artists=1&limit_albums=1&limit_tracks=1&buckets=artists'
		);
	});

	it('requires a signed-in user and at least two characters', () => {
		expect(getUnifiedSearchV3QueryOptions('user-1', 'ab').enabled).toBe(true);
		expect(getUnifiedSearchV3QueryOptions('user-1', 'a').enabled).toBe(false);
		expect(getUnifiedSearchV3QueryOptions(undefined, 'abba').enabled).toBe(false);
		expect(getSearchBucketV3QueryOptions('user-1', 'albums', '', 24, 0).enabled).toBe(false);
	});

	it('pages one bucket for drill-down views', async () => {
		const options = getSearchBucketV3QueryOptions('user-1', 'albums', 'abba', 24, 48);
		expect(options.queryKey).toEqual(
			SearchQueryKeyFactory.v3.bucket('user-1', 'albums', 'abba', 24, 48)
		);
		await callQueryFn(options, {});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/search/albums?q=abba&limit=24&offset=48',
			expect.anything()
		);
	});

	it('fetches typeahead suggestions with the caller gate applied', async () => {
		const query = getSearchSuggestionsV3Query(
			() => 'ab',
			() => true,
			5
		) as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
			enabled: boolean;
		};
		expect(query.enabled).toBe(true);
		expect(query.queryKey).toEqual(SearchQueryKeyFactory.v3.suggest('user-1', 'ab', 5));
		await query.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/search/suggest?q=ab&limit=5',
			expect.anything()
		);
		const gated = getSearchSuggestionsV3Query(
			() => 'abba',
			() => false
		) as unknown as {
			enabled: boolean;
		};
		expect(gated.enabled).toBe(false);
	});

	it('enriches one mixed batch with a single POST', async () => {
		const body = {
			artists: [{ musicbrainz_id: 'mbid-a' }],
			albums: [{ musicbrainz_id: 'rg-1', album_name: 'Album', artist_name: 'Artist' }]
		};
		const options = getSearchEnrichBatchV3QueryOptions('user-1', body);
		expect(options.queryKey).toEqual(
			SearchQueryKeyFactory.v3.enrich('user-1', { artists: ['mbid-a'], albums: ['rg-1'] })
		);
		await callQueryFn(options, {});
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/search/enrich/batch', body, {
			signal: undefined
		});
		expect(getSearchEnrichBatchV3QueryOptions('user-1', { artists: [], albums: [] }).enabled).toBe(
			false
		);
	});

	it('holds a degraded unified search briefly and clean answers for the full window', () => {
		const clean: SearchResponseV3 = {
			artists: [],
			albums: [],
			tracks: [],
			artist_status: 'ok',
			album_status: 'ok',
			track_status: 'ok'
		};
		expect(successfulUnifiedSearchV3StaleTime({ state: { data: clean } })).toBe(CACHE_TTL.SEARCH);
		const degraded: SearchResponseV3 = {
			...clean,
			artist_status: 'timeout'
		};
		expect(successfulUnifiedSearchV3StaleTime({ state: { data: degraded } })).toBe(60_000);
		expect(successfulUnifiedSearchV3StaleTime({ state: {} })).toBe(60_000);
	});

	it('holds degraded enrichment briefly and clean answers for the full window', () => {
		const clean: EnrichmentResponseV3 = {
			albums: [],
			artists: [],
			degradations: [],
			source: 'listenbrainz'
		};
		expect(successfulEnrichStaleTime({ state: { data: clean } })).toBe(CACHE_TTL.SEARCH);
		const degraded: EnrichmentResponseV3 = {
			albums: [],
			artists: [],
			degradations: [{ code: 'PROVIDER_DOWN', message: 'down', source: 'mb' }],
			source: 'listenbrainz'
		};
		expect(successfulEnrichStaleTime({ state: { data: degraded } })).toBe(
			ENRICH_DEGRADED_STALE_TIME_MS
		);
	});
});
