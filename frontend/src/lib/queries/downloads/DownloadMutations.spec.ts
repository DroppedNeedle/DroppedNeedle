import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));
vi.mock('$lib/api/client', () => ({
	api: {
		global: { post: vi.fn().mockResolvedValue(undefined), v3: { POST: vi.fn() } }
	}
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined),
	setQueryDataWithPersister: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'u1' } }
}));
vi.mock('$lib/stores/library', () => ({
	libraryStore: { addRequested: vi.fn() }
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import {
	reimportDownloadV3,
	reverifyHeldBulk,
	reverifyHeldTrack,
	type HeldBulkReverifyResponse,
	type HeldReverifyResponse
} from '$lib/queries/downloads/DownloadMutations.svelte';
import { DOWNLOAD_TASKS_ENDPOINTS } from '$lib/queries/downloads/endpoints';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { RequestQueryKeyFactory } from '$lib/queries/requests/RequestQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

const mockPost = vi.mocked(api.global.post);
const mockV3Post = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);
const mockToast = vi.mocked(toastStore.show);

interface Mutation<TData, TVars> {
	mutationFn: (vars: TVars) => Promise<TData>;
	onSuccess: (data: TData, vars: TVars) => void;
	onError: (err: unknown) => void;
}

type SingleVars = { id: number; release_group_mbid?: string | null };

describe('reverifyHeldTrack', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		captured.current = null;
	});

	it('posts to the single-reverify endpoint and refreshes tasks, held, and album on import', async () => {
		reverifyHeldTrack();
		const mutation = captured.current as unknown as Mutation<HeldReverifyResponse, SingleVars>;
		mockPost.mockResolvedValue({ status: 'imported', final_path: '/music/x.flac' });

		const data = await mutation.mutationFn({ id: 7, release_group_mbid: 'rg-1' });

		expect(mockPost).toHaveBeenCalledWith(API.downloads.heldReverify(7), {});
		expect(data).toEqual({ status: 'imported', final_path: '/music/x.flac' });

		mutation.onSuccess(data, { id: 7, release_group_mbid: 'rg-1' });

		expect(mockToast).toHaveBeenCalledWith({
			message: 'Re-check confirmed it: imported',
			type: 'success'
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('u1')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.heldPrefix('u1')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.album('rg-1')
		});
	});

	it('reports a still-held track without claiming an import', async () => {
		reverifyHeldTrack();
		const mutation = captured.current as unknown as Mutation<HeldReverifyResponse, SingleVars>;
		mockPost.mockResolvedValue({ status: 'still_held', final_path: null });

		const data = await mutation.mutationFn({ id: 7, release_group_mbid: 'rg-1' });
		mutation.onSuccess(data, { id: 7, release_group_mbid: 'rg-1' });

		expect(mockToast).toHaveBeenCalledWith({
			message: 'Still held after re-check',
			type: 'info'
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.heldPrefix('u1')
		});
		expect(mockInvalidate).not.toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.album('rg-1')
		});
	});
	it('toasts a failure without invalidating', () => {
		reverifyHeldTrack();
		const mutation = captured.current as unknown as Mutation<HeldReverifyResponse, SingleVars>;

		mutation.onError(new Error('nope'));

		expect(mockToast).toHaveBeenCalledWith({
			message: 'nope',
			type: 'error'
		});
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});

describe('reverifyHeldBulk', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		captured.current = null;
	});

	it('passes held_ids through and invalidates only imported albums', async () => {
		reverifyHeldBulk();
		const mutation = captured.current as unknown as Mutation<
			HeldBulkReverifyResponse,
			{ held_ids?: number[] | null }
		>;
		const response: HeldBulkReverifyResponse = {
			results: [
				{
					held_id: 1,
					status: 'imported',
					final_path: '/music/a.flac',
					release_group_mbid: 'rg-1',
					message: null
				},
				{
					held_id: 2,
					status: 'still_held',
					final_path: null,
					release_group_mbid: 'rg-2',
					message: null
				}
			]
		};
		mockPost.mockResolvedValue(response);

		const data = await mutation.mutationFn({ held_ids: [1, 2] });

		expect(mockPost).toHaveBeenCalledWith(API.downloads.heldReverifyBulk(), {
			held_ids: [1, 2]
		});
		expect(data).toEqual(response);

		mutation.onSuccess(data, { held_ids: [1, 2] });

		expect(mockToast).toHaveBeenCalledWith({
			message: 'Re-checked 2: 1 imported, 1 still held',
			type: 'success'
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('u1')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.heldPrefix('u1')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.album('rg-1')
		});
		expect(mockInvalidate).not.toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.album('rg-2')
		});
	});

	it('says so when there is nothing to re-check', () => {
		reverifyHeldBulk();
		const mutation = captured.current as unknown as Mutation<
			HeldBulkReverifyResponse,
			{ held_ids?: number[] | null }
		>;

		mutation.onSuccess({ results: [] }, { held_ids: [] });

		expect(mockToast).toHaveBeenCalledWith({ message: 'Nothing to re-check', type: 'info' });
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.heldPrefix('u1')
		});
	});

	it('toasts a failure without invalidating', () => {
		reverifyHeldBulk();
		const mutation = captured.current as unknown as Mutation<
			HeldBulkReverifyResponse,
			{ held_ids?: number[] | null }
		>;

		mutation.onError(new Error('bulk nope'));

		expect(mockToast).toHaveBeenCalledWith({ message: 'bulk nope', type: 'error' });
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});

describe('reimportDownloadV3', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		captured.current = null;
	});

	it('posts the v3 reimport and sweeps tasks, requests, and album on success', async () => {
		reimportDownloadV3();
		const mutation = captured.current as unknown as Mutation<
			{ success: boolean; status: string },
			{ id: string; release_group_mbid?: string | null }
		>;
		mockV3Post.mockResolvedValue({ success: true, status: 'queued' });

		const data = await mutation.mutationFn({ id: 'task-1', release_group_mbid: 'rg-1' });

		expect(mockV3Post).toHaveBeenCalledWith(DOWNLOAD_TASKS_ENDPOINTS.reimport('task-1'));
		expect(data).toEqual({ success: true, status: 'queued' });

		mutation.onSuccess(
			{ success: true, status: 'queued' },
			{ id: 'task-1', release_group_mbid: 'rg-1' }
		);

		expect(mockToast).toHaveBeenCalledWith({
			message: 'Back in line - checking the downloads mount again',
			type: 'success'
		});
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: DownloadQueryKeyFactory.tasks('u1')
		});
		expect(mockInvalidate).toHaveBeenCalledWith({ queryKey: RequestQueryKeyFactory.all });
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.album('rg-1')
		});
	});

	it('toasts the server message when the requeue is refused', () => {
		reimportDownloadV3();
		const mutation = captured.current as unknown as Mutation<
			{ success: boolean; status: string; error_message?: string | null },
			{ id: string }
		>;

		mutation.onSuccess({ success: false, status: 'failed' }, { id: 'task-1' });

		expect(mockToast).toHaveBeenCalledWith({
			message: "Couldn't requeue that download",
			type: 'error'
		});
	});

	it('toasts a failure without invalidating', () => {
		reimportDownloadV3();
		const mutation = captured.current as unknown as Mutation<unknown, { id: string }>;

		mutation.onError(new Error('reimport nope'));

		expect(mockToast).toHaveBeenCalledWith({ message: 'reimport nope', type: 'error' });
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});
