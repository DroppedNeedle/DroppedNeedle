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
	api: {
		global: {
			put: vi.fn(),
			v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() }
		}
	}
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { queryClient } from '../QueryClient';
import { ConnectionsQueryKeyFactory } from './ConnectionsQueryKeyFactory';
import { getConnectionsQuery } from './ConnectionsQuery.svelte';
import { createDisconnectMutation } from './ConnectionsMutations.svelte';

const mockPut = vi.mocked(api.global.put);
// The typed client's generics resolve mock results to void; loosen to Mock
// so resolves/implementations typecheck (assertions still pin URLs + bodies).
const mockV3Get = vi.mocked(api.global.v3.GET) as unknown as Mock;
const mockV3Post = vi.mocked(api.global.v3.POST) as unknown as Mock;
const mockV3Put = vi.mocked(api.global.v3.PUT) as unknown as Mock;
const mockV3Delete = vi.mocked(api.global.v3.DELETE) as unknown as Mock;

type Opts = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	mutationFn: (vars: unknown) => Promise<unknown>;
	onSuccess?: (data: unknown) => Promise<void> | void;
};

beforeEach(() => {
	vi.clearAllMocks();
	(authStore as { user: { id: string } | null }).user = { id: 'userA' };
	mockV3Get.mockRejectedValue(new Error('404'));
	mockV3Post.mockResolvedValue({});
	mockV3Put.mockResolvedValue({});
	mockV3Delete.mockResolvedValue({});
	mockPut.mockResolvedValue({});
});

describe('ConnectionsQueryKeyFactory (AMU-5)', () => {
	it('scopes the key by userId and normalizes a missing id to null', () => {
		expect(ConnectionsQueryKeyFactory.list('userA')).toEqual(['me', 'connections', 'userA']);
		expect(ConnectionsQueryKeyFactory.list(undefined)).toEqual(['me', 'connections', null]);
		expect(ConnectionsQueryKeyFactory.list('userB')).not.toEqual(
			ConnectionsQueryKeyFactory.list('userA')
		);
	});
});

describe('getConnectionsQuery', () => {
	it('builds a userId-scoped key', () => {
		expect((getConnectionsQuery() as unknown as Opts).queryKey).toEqual([
			'me',
			'connections',
			'userA'
		]);
	});

	it('does not leak across a user switch (key re-derives from authStore)', () => {
		expect((getConnectionsQuery() as unknown as Opts).queryKey).toEqual([
			'me',
			'connections',
			'userA'
		]);
		(authStore as { user: { id: string } | null }).user = { id: 'userB' };
		expect((getConnectionsQuery() as unknown as Opts).queryKey).toEqual([
			'me',
			'connections',
			'userB'
		]);
	});
});

describe('mutation onSuccess invalidates the user-scoped key', () => {
	it('disconnect invalidates ["me","connections","userA"]', async () => {
		const spy = vi.spyOn(queryClient, 'invalidateQueries');
		const m = createDisconnectMutation() as unknown as Opts;
		await m.onSuccess!({ service: 'lastfm', deleted: true });
		expect(spy.mock.calls[0][0]).toEqual(
			expect.objectContaining({ queryKey: ['me', 'connections', 'userA'] })
		);
		spy.mockRestore();
	});
});
