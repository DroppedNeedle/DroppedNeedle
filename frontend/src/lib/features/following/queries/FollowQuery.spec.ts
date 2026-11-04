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

vi.mock('$lib/queries/QueryClient', () => {
	const keyOf = (key: unknown) => JSON.stringify(key);
	const fakeClient = {
		getQueryData: vi.fn(
			<T = unknown>(key: unknown): T | undefined => queryCache.map.get(keyOf(key)) as T | undefined
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
	api: { v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn() } }
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
import { queryClient } from '$lib/queries/QueryClient';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import {
	getUnseenConcertsCountQuery,
	getUnseenNewReleasesCountQuery
} from './FollowQueries.svelte';
import { createSetAutoDownloadMutation } from './FollowMutations.svelte';
import type { FollowStatus } from './types';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock;
const mockPut = vi.mocked(api.v3.PUT) as unknown as Mock;
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

describe('unseen counts', () => {
	it('key each count to the signed-in user, and stay off while nobody is', () => {
		const counts: [() => unknown, (userId: string) => readonly unknown[]][] = [
			[getUnseenNewReleasesCountQuery, FollowQueryKeyFactory.newReleasesUnseen],
			[getUnseenConcertsCountQuery, FollowQueryKeyFactory.concertsUnseen]
		];
		for (const [query, key] of counts) {
			for (const id of ['userA', 'userB']) {
				auth.user = { id };
				const opts = query() as Opts & { enabled?: boolean };
				expect(opts.queryKey).toEqual(key(id));
				expect(opts.enabled).toBe(true);
			}
			auth.user = null;
			expect((query() as { enabled?: boolean }).enabled).toBe(false);
		}
	});
});

describe('follow mutations', () => {
	it('auto-download goes pending for a user and approved for an admin before the answer', async () => {
		for (const [isAdmin, state] of [
			[false, 'pending'],
			[true, 'approved']
		] as const) {
			queryClient.clear();
			auth.isAdmin = isAdmin;
			const m = createSetAutoDownloadMutation(() => MBID) as unknown as Opts;
			await m.onMutate!(true);
			const cached = queryClient.getQueryData<FollowStatus>(
				FollowQueryKeyFactory.status('userA', MBID)
			);
			expect(cached?.auto_download).toBe(true);
			expect(cached?.auto_download_state).toBe(state);
		}
	});
});
