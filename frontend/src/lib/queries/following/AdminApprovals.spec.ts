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
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn() } } }
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';
import { queryClient } from '../QueryClient';
import { FollowQueryKeyFactory } from './FollowQueryKeyFactory';
import { FOLLOW_ENDPOINTS } from './endpoints';
import { getAutoDownloadApprovalsQuery } from './AdminApprovalsQueries.svelte';
import {
	createApproveAutoDownloadMutation,
	createRejectAutoDownloadMutation
} from './AdminApprovalsMutations.svelte';

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
	mutationFn: (vars: unknown) => Promise<unknown>;
	onSuccess?: (data: unknown, vars: unknown) => Promise<void> | void;
};

const VARS = { userId: 'user-a', mbid: 'artist-1', artistName: 'Radiohead' };

beforeEach(() => {
	vi.clearAllMocks();
	mockGet.mockResolvedValue({ items: [], count: 0 });
	mockPost.mockResolvedValue({ success: true, message: 'ok' });
});

describe('admin auto-download approvals query', () => {
	it('uses the global admin key and hits the approvals endpoint', async () => {
		const opts = getAutoDownloadApprovalsQuery(() => true) as unknown as Opts;
		expect(opts.queryKey).toEqual(['following', 'admin-approvals']);
		expect(opts.enabled).toBe(true);
		await opts.queryFn!({ signal: new AbortController().signal });
		expect(mockGet.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.adminApprovals());
	});

	it('is disabled for non-admins / inactive tab', () => {
		const opts = getAutoDownloadApprovalsQuery(() => false) as unknown as Opts;
		expect(opts.enabled).toBe(false);
	});
});

describe('admin approval mutations', () => {
	it('approve POSTs the approve endpoint and toasts + invalidates', async () => {
		const spy = vi.spyOn(queryClient, 'invalidateQueries');
		const m = createApproveAutoDownloadMutation() as unknown as Opts;
		await m.mutationFn(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.approve('user-a', 'artist-1'));
		await m.onSuccess!({ success: true, message: 'ok' }, VARS);
		expect(mockShow).toHaveBeenCalledWith(
			expect.objectContaining({ type: 'success', message: expect.stringContaining('Radiohead') })
		);
		expect(spy.mock.calls[0][0]).toEqual(
			expect.objectContaining({ queryKey: FollowQueryKeyFactory.adminApprovals() })
		);
		spy.mockRestore();
	});

	it('reject POSTs the reject endpoint and toasts', async () => {
		const m = createRejectAutoDownloadMutation() as unknown as Opts;
		await m.mutationFn(VARS);
		expect(mockPost.mock.calls[0][0]).toBe(FOLLOW_ENDPOINTS.reject('user-a', 'artist-1'));
		await m.onSuccess!({ success: true, message: 'ok' }, VARS);
		expect(mockShow).toHaveBeenCalledWith(
			expect.objectContaining({ type: 'info', message: expect.stringContaining('Radiohead') })
		);
	});
});
