import { describe, expect, it, vi, beforeEach } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

// In-memory stand-in for ../QueryClient: the real module instantiates the
// QueryClient class from @tanstack/svelte-query, which a plain-object mock
// cannot re-export (a factory re-importing the original crashes the browser
// worker). This fake preserves what the tests observe: set/get round-trips,
// clear/ensure caching, and invalidation via queryClient.invalidateQueries.
const queryCache = vi.hoisted(() => ({ map: new Map<string, unknown>() }));

vi.mock('../QueryClient', () => {
	const keyOf = (key: unknown) => JSON.stringify(key);
	const fakeClient = {
		getQueryData: vi.fn(
			<T = unknown>(key: unknown): T | undefined =>
				queryCache.map.get(keyOf(key)) as T | undefined
		),
		setQueryData: vi.fn((key: unknown, updater: unknown) => {
			const next =
				typeof updater === 'function'
					? (updater as (old: unknown) => unknown)(queryCache.map.get(keyOf(key)))
					: updater;
			queryCache.map.set(keyOf(key), next);
			return next;
		}),
		removeQueries: vi.fn((filters?: { queryKey?: unknown }) => {
			if (filters?.queryKey === undefined) {
				queryCache.map.clear();
				return;
			}
			const prefix = keyOf(filters.queryKey).slice(0, -1);
			for (const k of [...queryCache.map.keys()]) {
				if (k.startsWith(prefix)) queryCache.map.delete(k);
			}
		}),
		invalidateQueries: vi.fn(async (_filters?: unknown, _options?: unknown) => undefined),
		cancelQueries: vi.fn(async (_filters?: unknown) => undefined),
		clear: vi.fn(() => queryCache.map.clear()),
		ensureQueryData: vi.fn(
			async (opts: {
				queryKey: unknown;
				queryFn: (ctx: { queryKey: unknown; signal: AbortSignal }) => Promise<unknown>;
			}): Promise<unknown> => {
				const k = keyOf(opts.queryKey);
				if (!queryCache.map.has(k)) {
					queryCache.map.set(
						k,
						await opts.queryFn({
							queryKey: opts.queryKey,
							signal: new AbortController().signal
						})
					);
				}
				return queryCache.map.get(k);
			}
		)
	};
	return {
		queryClient: fakeClient,
		invalidateQueriesWithPersister: vi.fn((filters?: unknown, options?: unknown) =>
			fakeClient.invalidateQueries(filters, options)
		),
		setQueryDataWithPersister: vi.fn(
			<_T = unknown>(key: unknown, updater: unknown): Promise<void> => {
				fakeClient.setQueryData(key, updater);
				return Promise.resolve();
			}
		)
	};
});

vi.mock('idb-keyval', () => ({
	get: vi.fn(),
	set: vi.fn(),
	del: vi.fn(),
	entries: vi.fn(async () => []),
	clear: vi.fn(),
	// Inert UseStore: persistence drops writes, like the get/set stubs above.
	createStore: vi.fn(() => vi.fn(async () => {}))
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn(), post: vi.fn(), put: vi.fn(), delete: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null, isAdmin: false },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { queryClient } from '../QueryClient';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { FOLLOW_ENDPOINTS } from './endpoints';
import {
	getCitySearchQuery,
	getConcertsQuery,
	getEventCitiesQuery,
	getFollowStatusQuery,
	getFollowedArtistsQuery,
	getRecentReleasesQuery,
	getUnseenConcertsCountQuery,
	getUnseenNewReleasesCountQuery
} from './FollowQueries.svelte';
import {
	createMarkConcertsSeenMutation,
	createMarkNewReleasesSeenMutation,
	createReplaceEventCitiesMutation,
	createSetAutoDownloadMutation,
	createSetFollowMutation
} from './FollowMutations.svelte';
import type { FollowStatus, UnseenCountResponse } from './types';

const mockGet = vi.mocked(api.global.get);
const mockPost = vi.mocked(api.global.post);
const mockPut = vi.mocked(api.global.put);
const mockShow = vi.mocked(toastStore.show);

type Opts = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn: (vars: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown) => Promise<{ prev: FollowStatus }>;
	onSuccess?: (data: unknown, vars: unknown) => Promise<void> | void;
};

const MBID = 'artist-1';
const auth = authStore as unknown as { user: { id: string } | null; isAdmin: boolean };

beforeEach(() => {
	vi.clearAllMocks();
	auth.user = { id: 'userA' };
	auth.isAdmin = false;
	mockGet.mockResolvedValue({ followed: false, auto_download: false, auto_download_state: 'none' });
	mockPut.mockResolvedValue({ followed: true, auto_download: false, auto_download_state: 'none' });
	queryClient.clear();
});

describe('FollowQueryKeyFactory (AMU-5)', () => {
	it('scopes every key by userId and falls back to anon', () => {
		expect(FollowQueryKeyFactory.status(MBID, 'userA')).toEqual([
			'follow',
			'status',
			MBID,
			'userA'
		]);
		expect(FollowQueryKeyFactory.status(MBID, undefined)).toEqual([
			'follow',
			'status',
			MBID,
			'anon'
		]);
		expect(FollowQueryKeyFactory.artists('userA')).toEqual(['following', 'artists', 'userA']);
		expect(FollowQueryKeyFactory.recentReleases('userA', 30, 48, true)).toEqual([
			'following',
			'recent-releases',
			'userA',
			30,
			48,
			true
		]);
		expect(FollowQueryKeyFactory.newReleasesUnseen('userA')).toEqual([
			'following',
			'new-releases-unseen',
			'userA'
		]);
		expect(FollowQueryKeyFactory.newReleasesUnseen(undefined)).toEqual([
			'following',
			'new-releases-unseen',
			'anon'
		]);
		expect(FollowQueryKeyFactory.status(MBID, 'userB')).not.toEqual(
			FollowQueryKeyFactory.status(MBID, 'userA')
		);
	});
});

describe('follow queries hit the right endpoints with user-scoped keys', () => {
	it('getFollowStatusQuery', async () => {
		const opts = getFollowStatusQuery(() => MBID) as unknown as Opts;
		expect(opts.queryKey).toEqual(['follow', 'status', MBID, 'userA']);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.status(MBID));
	});

	it('getFollowedArtistsQuery', async () => {
		const opts = getFollowedArtistsQuery() as unknown as Opts;
		expect(opts.queryKey).toEqual(['following', 'artists', 'userA']);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.followedArtists());
	});

	it('getRecentReleasesQuery threads window/limit/owned into key and URL', async () => {
		const opts = getRecentReleasesQuery(
			() => 30,
			() => 48,
			() => false
		) as unknown as Opts;
		expect(opts.queryKey).toEqual(['following', 'recent-releases', 'userA', 30, 48, false]);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.recentReleases(30, 48, false));
	});

	it('getUnseenNewReleasesCountQuery is user-scoped and disabled when logged out', async () => {
		const opts = getUnseenNewReleasesCountQuery() as unknown as Opts & { enabled?: boolean };
		expect(opts.queryKey).toEqual(['following', 'new-releases-unseen', 'userA']);
		expect(opts.enabled).toBe(true);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.newReleasesUnseenCount());

		auth.user = null;
		const loggedOut = getUnseenNewReleasesCountQuery() as unknown as { enabled?: boolean };
		expect(loggedOut.enabled).toBe(false);
	});
});

describe('concerts queries hit the right endpoints with user-scoped keys', () => {
	it('getConcertsQuery', async () => {
		const opts = getConcertsQuery() as unknown as Opts;
		expect(opts.queryKey).toEqual(['following', 'concerts', 'userA']);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.concerts());
	});

	it('getEventCitiesQuery', async () => {
		const opts = getEventCitiesQuery() as unknown as Opts;
		expect(opts.queryKey).toEqual(['following', 'concert-cities', 'userA']);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.concertCities());
	});

	it('getCitySearchQuery is enabled only from 2 characters', async () => {
		const short = getCitySearchQuery(() => 'l') as unknown as { enabled?: boolean };
		expect(short.enabled).toBe(false);
		const opts = getCitySearchQuery(() => 'liverpool') as unknown as Opts & {
			enabled?: boolean;
		};
		expect(opts.enabled).toBe(true);
		expect(opts.queryKey).toEqual(['following', 'city-search', 'userA', 'liverpool']);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.concertCitySearch('liverpool'));
	});

	it('getUnseenConcertsCountQuery is user-scoped and disabled when logged out', async () => {
		const opts = getUnseenConcertsCountQuery() as unknown as Opts & { enabled?: boolean };
		expect(opts.queryKey).toEqual(['following', 'concerts-unseen', 'userA']);
		expect(opts.enabled).toBe(true);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.concertsUnseenCount());

		auth.user = null;
		const loggedOut = getUnseenConcertsCountQuery() as unknown as { enabled?: boolean };
		expect(loggedOut.enabled).toBe(false);
	});
});

describe('concerts mutations', () => {
	it('replaceEventCities PUTs the full list and refreshes cities cache', async () => {
		const cities = [{ city_name: 'Liverpool', latitude: 53.41, longitude: -2.98, radius_km: 32 }];
		mockPut.mockResolvedValue({ items: cities });
		const m = createReplaceEventCitiesMutation() as unknown as Opts;
		await m.mutationFn(cities);
		expect(mockPut).toHaveBeenCalledWith(FOLLOW_ENDPOINTS.concertCities(), { items: cities });
		await m.onSuccess!({ items: cities }, cities);
		const cached = queryClient.getQueryData(FollowQueryKeyFactory.concertCities('userA'));
		expect(cached).toEqual({ items: cities });
	});

	it('replaceEventCities toasts on error', async () => {
		const m = createReplaceEventCitiesMutation() as unknown as Opts & {
			onError?: () => void;
		};
		m.onError!();
		expect(mockShow).toHaveBeenCalledWith(expect.objectContaining({ type: 'error' }));
	});

	it('markConcertsSeen POSTs and zeroes the cached badge count', async () => {
		mockPost.mockResolvedValue({ count: 0 });
		const m = createMarkConcertsSeenMutation() as unknown as Opts;
		await m.mutationFn(undefined);
		expect(mockPost).toHaveBeenCalledWith(FOLLOW_ENDPOINTS.markConcertsSeen());
		await m.onSuccess!({ count: 0 }, undefined);
		const cached = queryClient.getQueryData<UnseenCountResponse>(
			FollowQueryKeyFactory.concertsUnseen('userA')
		);
		expect(cached).toEqual({ count: 0 });
	});
});

describe('follow mutations', () => {
	it('setFollow PUTs { followed } to the follow endpoint', async () => {
		const m = createSetFollowMutation(() => MBID) as unknown as Opts;
		await m.mutationFn(true);
		expect(mockPut).toHaveBeenCalledWith(FOLLOW_ENDPOINTS.setFollow(MBID), { followed: true });
	});

	it('setAutoDownload PUTs { enabled } to the auto-download endpoint', async () => {
		const m = createSetAutoDownloadMutation(() => MBID) as unknown as Opts;
		await m.mutationFn(true);
		expect(mockPut).toHaveBeenCalledWith(FOLLOW_ENDPOINTS.autoDownload(MBID), { enabled: true });
	});

	it('non-admin auto-download optimistically goes pending (D3)', async () => {
		auth.isAdmin = false;
		const m = createSetAutoDownloadMutation(() => MBID) as unknown as Opts;
		await m.onMutate!(true);
		const cached = queryClient.getQueryData<FollowStatus>(
			FollowQueryKeyFactory.status(MBID, 'userA')
		);
		expect(cached?.auto_download).toBe(true);
		expect(cached?.auto_download_state).toBe('pending');
	});

	it('admin auto-download optimistically goes approved (D3)', async () => {
		auth.isAdmin = true;
		const m = createSetAutoDownloadMutation(() => MBID) as unknown as Opts;
		await m.onMutate!(true);
		const cached = queryClient.getQueryData<FollowStatus>(
			FollowQueryKeyFactory.status(MBID, 'userA')
		);
		expect(cached?.auto_download_state).toBe('approved');
	});

	it('shows the pending toast when the server confirms pending', async () => {
		const m = createSetAutoDownloadMutation(() => MBID) as unknown as Opts;
		await m.onSuccess!(
			{ followed: true, auto_download: true, auto_download_state: 'pending' },
			true
		);
		expect(mockShow).toHaveBeenCalledWith(
			expect.objectContaining({ type: 'info', message: expect.stringContaining('admin') })
		);
	});

	it('markNewReleasesSeen POSTs and zeroes the cached badge count', async () => {
		mockPost.mockResolvedValue({ count: 0 });
		const m = createMarkNewReleasesSeenMutation() as unknown as Opts;
		await m.mutationFn(undefined);
		expect(mockPost).toHaveBeenCalledWith(FOLLOW_ENDPOINTS.markNewReleasesSeen());
		await m.onSuccess!({ count: 0 }, undefined);
		const cached = queryClient.getQueryData<UnseenCountResponse>(
			FollowQueryKeyFactory.newReleasesUnseen('userA')
		);
		expect(cached).toEqual({ count: 0 });
	});

	it('does not toast when an admin is approved immediately', async () => {
		const m = createSetAutoDownloadMutation(() => MBID) as unknown as Opts;
		await m.onSuccess!(
			{ followed: true, auto_download: true, auto_download_state: 'approved' },
			true
		);
		expect(mockShow).not.toHaveBeenCalled();
	});
});
