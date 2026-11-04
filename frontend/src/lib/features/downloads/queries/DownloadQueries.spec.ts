import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => unknown) => factory()),
	queryOptions: vi.fn((opts: unknown) => opts)
}));

const mockGet = vi.fn();
const mockPost = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: {
		v3: {
			GET: (...args: unknown[]) => mockGet(...args),
			POST: (...args: unknown[]) => mockPost(...args)
		}
	}
}));

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => unknown) => factory())
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
import { createRequestAlbumMutation, createRequestBatchMutation } from './DownloadMutations.svelte';

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
		mockInvalidate.mockClear();
		mockToast.mockClear();
		mockAddRequested.mockClear();
	});

	it('polls the compact summary every 60 s and gives the detailed list no competing interval', async () => {
		const summary = getDownloadActivitySummaryQueryOptions() as unknown as {
			queryFn: (a: { signal?: AbortSignal }) => unknown;
			refetchInterval: number;
			refetchIntervalInBackground: boolean;
			refetchOnReconnect: string;
			refetchOnWindowFocus: string | undefined;
			staleTime: number;
		};
		const list = getDownloadsQueryOptions() as unknown as {
			refetchInterval?: number;
			refetchOnWindowFocus: string | undefined;
			refetchOnReconnect: string | undefined;
			staleTime: number;
		};

		await summary.queryFn({ signal: undefined });

		expect(mockGet.mock.calls.at(-1)?.[0]).toBe('/api/v3/downloads/activity-summary');
		expect(summary).toMatchObject({
			refetchInterval: 60_000,
			refetchIntervalInBackground: false,
			refetchOnReconnect: 'always',
			staleTime: 0
		});
		// Invalidations and the interval own freshness, so no focus refetch.
		expect(summary.refetchOnWindowFocus).toBeUndefined();
		// The list keeps the default stale window and no always-flag.
		for (const option of [
			list.refetchInterval,
			list.refetchOnWindowFocus,
			list.refetchOnReconnect,
			list.staleTime
		]) {
			expect(option).toBeUndefined();
		}
	});

	it('drops album and batch badge writes when the account changes before the response', async () => {
		const cases: [() => unknown, unknown, unknown, Record<string, unknown>][] = [
			[
				createRequestAlbumMutation,
				{ release_group_mbid: 'release-a' },
				{ success: true, message: '', musicbrainz_id: 'release-a', status: 'pending' },
				{ success: false }
			],
			[
				createRequestBatchMutation,
				{ items: [{ musicbrainz_id: 'release-a' }] },
				{ success: true, message: 'ok', requested: 1, skipped: 0, overflow: 0 },
				{ success: false, requested: 1 }
			]
		];
		for (const [create, input, answer, expected] of cases) {
			authStoreUser.current.id = 'user-1';
			const { promise, resolve } = deferred<unknown>();
			mockPost.mockReturnValueOnce(promise);
			const m = create() as { mutationFn: (i: unknown) => Promise<unknown> };

			const pending = m.mutationFn(input);
			authStoreUser.current.id = 'user-b';
			resolve(answer);

			expect(await pending).toMatchObject(expected);
			expect(mockAddRequested).not.toHaveBeenCalled();
			expect(mockToast).not.toHaveBeenCalled();
		}
	});
});
