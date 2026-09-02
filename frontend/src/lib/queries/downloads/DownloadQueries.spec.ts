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
			put: (...args: unknown[]) => mockPut(...args)
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
import {
	cancelDownload,
	requestAlbum,
	requestBatch,
	requestTrack,
	retryHeldManagementUnit,
	retryDownload,
	tryNextSource
} from './DownloadMutations.svelte';
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

	it('the downloads list query hits /api/v1/downloads', async () => {
		const opts = getDownloadsQueryOptions() as { queryFn: (a: unknown) => unknown };

		await opts.queryFn({ signal: undefined });
		expect(String(mockGet.mock.calls.at(-1)?.[0])).toContain('/api/v1/downloads');
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
		expect(opts.queryKey).toEqual(['downloads', 'tasks', 'user-1', 'activity']);
		expect(opts.refetchInterval({ state: { data: { active_count: 1 } } })).toBe(750);
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

	it('requestAlbum posts to /requests/new with the mapped body', async () => {
		mockPost.mockResolvedValueOnce({
			success: true,
			message: 'Request accepted',
			musicbrainz_id: 'rg',
			status: 'pending'
		});
		const m = requestAlbum() as unknown as { mutationFn: (i: unknown) => Promise<unknown> };
		await m.mutationFn({
			release_group_mbid: 'rg',
			artist_name: 'A',
			album_title: 'B',
			year: 2000
		});
		const call = mockPost.mock.calls.at(-1);
		expect(String(call?.[0])).toContain('/requests/new');
		expect(call?.[1]).toMatchObject({ musicbrainz_id: 'rg', artist: 'A', album: 'B', year: 2000 });
	});

	it('requestTrack posts the complete exact-track payload', async () => {
		const m = requestTrack() as unknown as { mutationFn: (i: unknown) => unknown };
		await m.mutationFn({
			recording_mbid: 'rec',
			artist_name: 'A',
			track_title: 'T',
			album_title: 'B',
			duration_seconds: 287,
			release_group_mbid: 'rg',
			artist_mbid: 'artist',
			release_id: 'release'
		});
		const call = mockPost.mock.calls.at(-1);
		expect(call?.[0]).toBe('/api/v1/tracks/rec/request');
		expect(call?.[1]).toEqual({
			artist_name: 'A',
			track_title: 'T',
			album_title: 'B',
			duration_seconds: 287,
			release_group_mbid: 'rg',
			artist_mbid: 'artist',
			release_id: 'release'
		});
	});

	it('cancelDownload posts to /downloads/{id}/cancel', async () => {
		const m = cancelDownload() as unknown as { mutationFn: (i: string) => unknown };
		await m.mutationFn('t1');
		expect(String(mockPost.mock.calls.at(-1)?.[0])).toContain('/downloads/t1/cancel');
	});

	it('retryDownload posts to /downloads/{id}/retry', async () => {
		const m = retryDownload() as unknown as { mutationFn: (i: string) => unknown };
		await m.mutationFn('t1');
		expect(String(mockPost.mock.calls.at(-1)?.[0])).toContain('/downloads/t1/retry');
	});

	it('tryNextSource posts the rendered candidate index to the task endpoint', async () => {
		const m = tryNextSource() as unknown as { mutationFn: (i: unknown) => unknown };
		await m.mutationFn({ id: 't1', candidateIndex: 4 });
		const call = mockPost.mock.calls.at(-1);
		expect(String(call?.[0])).toContain('/downloads/t1/next-source');
		expect(call?.[1]).toEqual({ expected_candidate_index: 4 });
	});

	it('tryNextSource reports success and conflicts through toasts', () => {
		mockToast.mockClear();
		const m = tryNextSource() as unknown as {
			onSuccess: () => unknown;
			onError: (error: unknown) => unknown;
		};

		m.onSuccess();
		expect(mockToast).toHaveBeenLastCalledWith({
			message: 'Trying the next source',
			type: 'info'
		});

		m.onError(new Error('The transfer has already started'));
		expect(mockToast).toHaveBeenLastCalledWith({
			message: 'The transfer has already started',
			type: 'error'
		});
	});

	it('refreshes held and task data immediately when organizer retry is rejected', () => {
		mockInvalidate.mockClear();
		const mutation = retryHeldManagementUnit() as unknown as {
			onError: (error: unknown) => unknown;
		};

		mutation.onError(new Error('Exact edition proof is incomplete.'));

		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('user-1')
		});
	});

	it.each([
		{
			status: 'pending',
			message: 'Request accepted',
			expectedMessage: 'Requested - searching now.',
			type: 'success'
		},
		{
			status: 'awaiting_approval',
			message: 'Request submitted, awaiting admin approval',
			expectedMessage:
				'Submitted for approval. The current server policy will apply when approved.',
			type: 'success'
		},
		{
			status: 'queued',
			message: 'Request already in progress',
			expectedMessage: 'Already being acquired.',
			type: 'info'
		},
		{
			status: 'downloading',
			message: 'Request already in progress',
			expectedMessage: 'Already being acquired.',
			type: 'info'
		},
		{
			status: 'pending',
			message: 'Album is already in the library',
			expectedMessage: 'Album is already in the library',
			type: 'info'
		},
		{
			status: 'cancelling',
			message: 'Request is being cancelled',
			expectedMessage: 'Request is being cancelled',
			type: 'info'
		}
	] as const)('requestAlbum shows the spec copy for $status responses', async (response) => {
		mockToast.mockClear();
		mockPost.mockResolvedValueOnce({
			success: true,
			message: response.message,
			musicbrainz_id: 'rg',
			status: response.status
		});
		const m = requestAlbum() as unknown as { mutationFn: (i: unknown) => Promise<unknown> };

		await m.mutationFn({ release_group_mbid: 'rg' });

		expect(mockAddRequested).toHaveBeenCalledWith('rg');
		expect(mockInvalidate).toHaveBeenCalledWith({ queryKey: ['downloads'] });
		expect(mockInvalidate).toHaveBeenCalledWith({ queryKey: ['requests'] });
		expect(mockToast).toHaveBeenLastCalledWith({
			message: response.expectedMessage,
			type: response.type
		});
	});

	it('requestAlbum names the snapshot summary in dispatched and duplicate copy when carried', async () => {
		for (const [status, expected] of [
			['pending', 'Requested - searching now using Balanced.'],
			['queued', 'Already being acquired using Balanced.']
		] as const) {
			mockToast.mockClear();
			mockPost.mockResolvedValueOnce({
				success: true,
				message: status === 'pending' ? 'Request accepted' : 'Request already in progress',
				musicbrainz_id: 'rg',
				status,
				quality_snapshot_summary: 'Balanced'
			});
			const m = requestAlbum() as unknown as { mutationFn: (i: unknown) => Promise<unknown> };
			await m.mutationFn({ release_group_mbid: 'rg' });
			expect(mockToast).toHaveBeenCalledWith({
				message: expected,
				type: status === 'pending' ? 'success' : 'info'
			});
		}
	});

	it('requestAlbum reports an unsuccessful response without touching badges', async () => {
		mockToast.mockClear();
		mockPost.mockResolvedValueOnce({
			success: false,
			message: 'Request could not be recorded',
			musicbrainz_id: 'rg',
			status: 'failed'
		});
		const m = requestAlbum() as unknown as { mutationFn: (i: unknown) => Promise<unknown> };

		const result = await m.mutationFn({ release_group_mbid: 'rg' });

		expect(result).toMatchObject({ success: false });
		expect(mockAddRequested).not.toHaveBeenCalled();
		expect(mockToast).toHaveBeenLastCalledWith({
			message: 'Request could not be recorded',
			type: 'error'
		});
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

	it('requestBatch posts the mapped payload and renders counts verbatim', async () => {
		mockPost.mockResolvedValueOnce({
			success: true,
			message: '',
			requested: 3,
			skipped: 1,
			overflow: 2
		});
		const m = requestBatch() as unknown as { mutationFn: (i: unknown) => Promise<unknown> };

		const result = await m.mutationFn({
			items: [{ musicbrainz_id: 'rg-1' }, { musicbrainz_id: 'rg-2' }],
			monitorArtist: true,
			autoDownloadArtist: false
		});

		const call = mockPost.mock.calls.at(-1);
		expect(String(call?.[0])).toBe('/api/v1/requests/batch');
		expect(call?.[1]).toEqual({
			items: [{ musicbrainz_id: 'rg-1' }, { musicbrainz_id: 'rg-2' }],
			monitor_artist: true,
			auto_download_artist: false
		});
		expect(result).toMatchObject({ success: true, requested: 3, skipped: 1, overflow: 2 });
		expect(mockToast).toHaveBeenLastCalledWith({
			message: '3 requested, 1 skipped, 2 were over the batch request limit',
			type: 'info'
		});
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

	it.each([
		['already_in_library', 'That track is already in your library'],
		['awaiting_approval', 'Track request submitted for admin approval'],
		['queued', 'Track requested - searching for downloads']
	] as const)('requestTrack shows the correct toast for %s', (status, message) => {
		mockToast.mockClear();
		const m = requestTrack() as unknown as { onSuccess: (d: unknown) => unknown };
		m.onSuccess({ status });
		expect(mockToast).toHaveBeenCalledWith({ message, type: 'success' });
	});

	it('the key factory builds stable keys', () => {
		expect(DownloadQueryKeyFactory.tasks('user-1')).toEqual(['downloads', 'tasks', 'user-1']);
		expect(DownloadQueryKeyFactory.tasks('user-2')).not.toEqual(
			DownloadQueryKeyFactory.tasks('user-1')
		);
		expect(DownloadQueryKeyFactory.activity('user-1')).toEqual([
			'downloads',
			'tasks',
			'user-1',
			'activity'
		]);
		expect(DownloadQueryKeyFactory.held('user-1')).toEqual([
			'downloads',
			'tasks',
			'user-1',
			'held',
			'all'
		]);
		expect(DownloadQueryKeyFactory.quarantine()).toEqual(['downloads', 'quarantine']);
	});
});
