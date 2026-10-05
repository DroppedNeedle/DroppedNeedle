import { beforeEach, describe, expect, it, vi } from 'vitest';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => unknown) => factory()),
	createMutation: vi.fn((factory: () => unknown) => factory()),
	queryOptions: vi.fn((opts: unknown) => opts)
}));

const mockGet = vi.fn();
const mockPost = vi.fn();
const mockPut = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: (...args: unknown[]) => mockGet(...args),
			post: (...args: unknown[]) => mockPost(...args),
			put: (...args: unknown[]) => mockPut(...args),
			v3: { POST: (...args: unknown[]) => mockPost(...args) }
		}
	}
}));

const { mockInvalidate } = vi.hoisted(() => ({ mockInvalidate: vi.fn() }));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: (...args: unknown[]) => mockInvalidate(...args)
}));
const authStoreUser = vi.hoisted(() => ({ current: { id: 'user-1' } }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: {
		get user() {
			return authStoreUser.current;
		}
	}
}));

const { mockToast, mockAddRequested } = vi.hoisted(() => ({
	mockToast: vi.fn(),
	mockAddRequested: vi.fn()
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: (...args: unknown[]) => mockToast(...args) }
}));

vi.mock('$lib/stores/library', () => ({
	libraryStore: { addRequested: (...args: unknown[]) => mockAddRequested(...args) }
}));

import {
	getDownloadActivitySummaryQueryOptions,
	getDownloadsQueryOptions
} from './DownloadQueries.svelte';
import { requestAlbum, requestBatch } from './DownloadMutations.svelte';
import { saveDownloadPolicy } from './DownloadClientsQueries.svelte';
describe('download queue queries', () => {
	// Promise.withResolvers needs Node 22+; this repo runs Node 20. The deferred
	// below keeps the same linear hand-off for the session-switch tests.
	function deferred<T>() {
		let resolve!: (value: T) => void;
		const promise = new Promise<T>((settle) => {
			resolve = settle;
		});
		return { promise, resolve };
	}

	beforeEach(() => {
		authStoreUser.current.id = 'user-1';
		mockGet.mockClear();
		mockPost.mockClear();
		mockPut.mockClear();
		mockInvalidate.mockClear();
		mockToast.mockClear();
		mockAddRequested.mockClear();
	});

	it('invalidates policy and policy summary with the persister after saving', async () => {
		mockInvalidate.mockClear();
		const mutation = saveDownloadPolicy() as unknown as {
			onSuccess: () => Promise<void>;
		};

		await mutation.onSuccess();

		expect(mockInvalidate).toHaveBeenNthCalledWith(1, {
			queryKey: DownloadQueryKeyFactory.policy()
		});
		expect(mockInvalidate).toHaveBeenNthCalledWith(2, {
			queryKey: DownloadQueryKeyFactory.policySummary()
		});
	});

	it('uses one visibility-aware compact summary owner with active and idle cadences', async () => {
		const opts = getDownloadActivitySummaryQueryOptions() as unknown as {
			queryFn: (a: { signal?: AbortSignal }) => unknown;
			queryKey: readonly unknown[];
			refetchInterval: (query: { state: { data?: { active_count: number } } }) => number;
			refetchIntervalInBackground: boolean;
			refetchOnReconnect: string;
			refetchOnWindowFocus: string | undefined;
			staleTime: number;
		};

		await opts.queryFn({ signal: undefined });

		expect(mockGet.mock.calls.at(-1)?.[0]).toBe('/api/v1/downloads/activity-summary');
		expect(opts.refetchInterval({ state: { data: { active_count: 1 } } })).toBe(5_000);
		expect(opts.refetchInterval({ state: { data: { active_count: 0 } } })).toBe(120_000);
		expect(opts.refetchIntervalInBackground).toBe(false);
		expect(opts.refetchOnReconnect).toBe('always');
		// B6: focus-'always' dropped - invalidations + the interval own freshness
		expect(opts.refetchOnWindowFocus).toBeUndefined();
		expect(opts.staleTime).toBe(0);
	});

	it('does not give the detailed downloads list a competing interval', () => {
		const opts = getDownloadsQueryOptions() as unknown as {
			refetchInterval?: number;
			refetchOnWindowFocus: string | undefined;
			refetchOnReconnect: string | undefined;
			staleTime: number;
		};

		expect(opts.refetchInterval).toBeUndefined();
		// B6: neither always-flag remains; 30 s stale window instead
		expect(opts.refetchOnWindowFocus).toBeUndefined();
		expect(opts.refetchOnReconnect).toBeUndefined();
		expect(opts.staleTime).toBe(30_000);
	});

	it('drops single-album badge writes when the account changes before the response', async () => {
		const { promise, resolve } = deferred<{
			success: boolean;
			message: string;
			musicbrainz_id: string;
			status: string;
		}>();
		mockPost.mockReturnValueOnce(promise);
		const m = requestAlbum() as unknown as {
			mutationFn: (i: unknown) => Promise<{ success: boolean }>;
		};

		const pending = m.mutationFn({ release_group_mbid: 'release-a' });
		authStoreUser.current.id = 'user-b';
		resolve({ success: true, message: '', musicbrainz_id: 'release-a', status: 'pending' });

		const result = await pending;
		expect(result.success).toBe(false);
		expect(mockAddRequested).not.toHaveBeenCalled();
		expect(mockToast).not.toHaveBeenCalled();
	});

	it('drops batch badge writes when the account changes before the response', async () => {
		const { promise, resolve } = deferred<{
			success: boolean;
			message: string;
			requested: number;
			skipped: number;
			overflow: number;
		}>();
		mockPost.mockReturnValueOnce(promise);
		const m = requestBatch() as unknown as {
			mutationFn: (i: unknown) => Promise<{ success: boolean; requested: number }>;
		};

		const pending = m.mutationFn({ items: [{ musicbrainz_id: 'release-a' }] });
		authStoreUser.current.id = 'user-b';
		resolve({ success: true, message: 'ok', requested: 1, skipped: 0, overflow: 0 });

		const result = await pending;
		expect(result).toMatchObject({ success: false, requested: 1 });
		expect(mockAddRequested).not.toHaveBeenCalled();
		expect(mockToast).not.toHaveBeenCalled();
	});
});
