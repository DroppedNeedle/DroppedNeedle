import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	keepPreviousData: 'keepPreviousData'
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

import { api } from '$lib/api/client';
import { LOCAL_V3_KEYS } from './LocalV3Keys';
import {
	getLocalAlbumMatchV3Query,
	getLocalAlbumsV3Query,
	getLocalDecadesV3Query,
	getLocalRecentV3Query,
	getLocalSearchV3Query,
	getLocalStatsV3Query,
	getLocalSuggestionsV3Query
} from './LocalV3Queries.svelte';
import { LocalV3Api } from './LocalV3Api';

beforeEach(() => vi.clearAllMocks());

describe('LocalV3Queries', () => {
	it('browses local albums with paging params on the shared v3 key', async () => {
		const params = { limit: 24, offset: 0, sort: 'recent', order: 'desc' } as const;
		const query = getLocalAlbumsV3Query(() => params) as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(query.queryKey).toEqual(LOCAL_V3_KEYS.albums(params));
		await query.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/local-library/albums?limit=24&offset=0&sort=recent&order=desc',
			expect.anything()
		);
	});

	it('nests every v3 local key under the local root without a userId segment', () => {
		const keys = [
			LOCAL_V3_KEYS.albums({ limit: 1, offset: 0, sort: 'recent', order: 'desc' }),
			LOCAL_V3_KEYS.recent(null),
			LOCAL_V3_KEYS.suggestions(16, null),
			LOCAL_V3_KEYS.search('abba', null),
			LOCAL_V3_KEYS.decades(),
			LOCAL_V3_KEYS.stats(),
			LOCAL_V3_KEYS.albumMatch('rg1', {})
		];
		for (const key of keys) {
			expect([...key].slice(0, 2)).toEqual(['local', 'v3']);
		}
	});

	it('requires two characters for local search and reuses prior data meanwhile', async () => {
		const query = getLocalSearchV3Query(() => 'ab') as unknown as {
			enabled: boolean;
			placeholderData: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(query.enabled).toBe(true);
		expect(query.placeholderData).toBe('keepPreviousData');
		await query.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/local-library/search?q=ab',
			expect.anything()
		);
		const short = getLocalSearchV3Query(() => 'a') as unknown as { enabled: boolean };
		expect(short.enabled).toBe(false);
	});

	it('never serves a stale suggestion crate', () => {
		const query = getLocalSuggestionsV3Query(() => undefined) as unknown as {
			staleTime: number;
			gcTime: number;
		};
		expect(query.staleTime).toBe(0);
		expect(query.gcTime).toBe(0);
	});

	it('reads recent shelves and decade shelves from v3', async () => {
		const recent = getLocalRecentV3Query(() => 12) as unknown as {
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		await recent.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/local-library/recent?limit=12',
			expect.anything()
		);
		const decades = getLocalDecadesV3Query() as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(decades.queryKey).toEqual(LOCAL_V3_KEYS.decades());
		await decades.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/local-library/decades',
			expect.anything()
		);
		expect(LocalV3Api.suggestions(16, 1990)).toBe(
			'/api/v3/local-library/suggestions?limit=16&decade=1990'
		);
	});

	it('reads library totals from the shared stats route', async () => {
		const stats = getLocalStatsV3Query() as unknown as {
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(stats.queryKey).toEqual(LOCAL_V3_KEYS.stats());
		await stats.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith('/api/v3/library/stats', expect.anything());
	});

	it('matches album tracks by mbid and stays disabled without one', async () => {
		const match = getLocalAlbumMatchV3Query(
			() => 'rg1',
			() => ({ limit: 1000 })
		) as unknown as {
			enabled: boolean;
			queryKey: unknown;
			queryFn: (args: { signal?: AbortSignal }) => Promise<unknown>;
		};
		expect(match.enabled).toBe(true);
		expect(match.queryKey).toEqual(LOCAL_V3_KEYS.albumMatch('rg1', { limit: 1000 }));
		await match.queryFn({});
		expect(api.global.v3.GET).toHaveBeenCalledWith(
			'/api/v3/local-library/albums/match/rg1?limit=1000',
			expect.anything()
		);
		const idle = getLocalAlbumMatchV3Query(() => '') as unknown as { enabled: boolean };
		expect(idle.enabled).toBe(false);
	});

	it('gates decade shelf browsing on an open decade', () => {
		const params = { limit: 50, offset: 0, sort: 'name', order: 'asc', decade: 1990 } as const;
		const open = getLocalAlbumsV3Query(
			() => params,
			() => true
		) as unknown as {
			enabled: boolean;
		};
		expect(open.enabled).toBe(true);
		const closed = getLocalAlbumsV3Query(
			() => params,
			() => false
		) as unknown as {
			enabled: boolean;
		};
		expect(closed.enabled).toBe(false);
	});
});
