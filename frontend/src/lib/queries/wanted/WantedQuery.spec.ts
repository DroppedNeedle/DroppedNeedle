import { describe, expect, it, vi, beforeEach, type Mock } from 'vitest';

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
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn() } } }
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
import { WantedQueryKeyFactory } from './WantedQueryKeyFactory';
import { WANTED_ENDPOINTS } from './endpoints';
import { getWantedWatchesQuery } from './WantedQuery.svelte';
import {
	createMarkWantedSeenMutation,
	createResumeWatchMutation,
	createStopWatchMutation,
	type WantedActionVars
} from './WantedMutations.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockShow = vi.mocked(toastStore.show);

type Opts = {
	queryKey?: unknown;
	enabled?: boolean;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn?: (vars: WantedActionVars) => Promise<unknown>;
	onSuccess?: (data: unknown, vars: WantedActionVars) => void;
	onError?: (err: unknown, vars: WantedActionVars) => void;
	onSettled?: () => void;
};

const MBID = '22222222-2222-2222-2222-222222222222';
const VARS: WantedActionVars = { mbid: MBID, albumTitle: 'the arrival' };
const auth = authStore as unknown as { user: { id: string } | null; isAdmin: boolean };

beforeEach(() => {
	vi.clearAllMocks();
	auth.user = { id: 'userA' };
	mockGet.mockResolvedValue({ items: [], count: 0, retrying: [] });
	mockPost.mockResolvedValue({ success: true, state: 'stopped' });
	queryClient.clear();
});

describe('WantedQueryKeyFactory', () => {
	it('scopes the list key by userId and falls back to null', () => {
		expect(WantedQueryKeyFactory.list('userA')).toEqual(['wanted', 'list', 'userA']);
		expect(WantedQueryKeyFactory.list(undefined)).toEqual(['wanted', 'list', null]);
		expect(WantedQueryKeyFactory.list('userB')).not.toEqual(WantedQueryKeyFactory.list('userA'));
	});
});

describe('getWantedWatchesQuery', () => {
	it('hits the list endpoint with a user-scoped key and forwards the signal', async () => {
		const opts = getWantedWatchesQuery(() => true) as unknown as Opts;
		expect(opts.queryKey).toEqual(['wanted', 'list', 'userA']);
		expect(opts.enabled).toBe(true);
		const signal = new AbortController().signal;
		await opts.queryFn!({ signal });
		expect(mockGet.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.list());
		expect(mockGet.mock.calls[0][1]).toEqual({ signal });
	});

	it('stays disabled while the tab is closed or nobody is signed in', () => {
		expect((getWantedWatchesQuery(() => false) as unknown as Opts).enabled).toBe(false);
		auth.user = null;
		expect((getWantedWatchesQuery(() => true) as unknown as Opts).enabled).toBe(false);
	});
});

describe('wanted mutations', () => {
	it('stop posts to the stop endpoint and toasts on success', async () => {
		const opts = createStopWatchMutation() as unknown as Opts;
		await opts.mutationFn!(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.stop(MBID));
		opts.onSuccess?.({ success: true, state: 'stopped' }, VARS);
		expect(mockShow).toHaveBeenCalledWith(
			expect.objectContaining({ message: expect.stringContaining('the arrival') })
		);
	});

	it('stop toasts an error on failure', () => {
		const opts = createStopWatchMutation() as unknown as Opts;
		opts.onError?.(new Error('nope'), VARS);
		expect(mockShow).toHaveBeenCalledWith(expect.objectContaining({ type: 'error' }));
	});

	it('resume posts to the resume endpoint', async () => {
		const opts = createResumeWatchMutation() as unknown as Opts;
		await opts.mutationFn!(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.resume(MBID));
	});

	it('mark-seen posts to the seen endpoint without a toast', async () => {
		const opts = createMarkWantedSeenMutation() as unknown as Opts;
		await opts.mutationFn!(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(WANTED_ENDPOINTS.seen(MBID));
		expect(mockShow).not.toHaveBeenCalled();
	});

	it('mark-seen refreshes the watchlist on settle', () => {
		const spy = vi.spyOn(queryClient, 'invalidateQueries');
		try {
			const opts = createMarkWantedSeenMutation() as unknown as Opts;
			opts.onSettled?.();
			expect(spy).toHaveBeenCalledWith(
				expect.objectContaining({ queryKey: WantedQueryKeyFactory.list('userA') }),
				undefined
			);
		} finally {
			spy.mockRestore();
		}
	});
});
