import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { post: vi.fn().mockResolvedValue(undefined) } }
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
	reverifyHeldBulk,
	reverifyHeldTrack,
	type HeldBulkReverifyResponse,
	type HeldReverifyResponse
} from '$lib/queries/downloads/DownloadMutations.svelte';
import { LibraryQueryKeyFactory } from '$lib/queries/library/LibraryQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

const mockPost = vi.mocked(api.global.post);
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
		const mutation = captured.current as Mutation<HeldReverifyResponse, SingleVars>;
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
		const mutation = captured.current as Mutation<HeldReverifyResponse, SingleVars>;
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
		const mutation = captured.current as Mutation<HeldReverifyResponse, SingleVars>;

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
		const mutation = captured.current as Mutation<
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
		const mutation = captured.current as Mutation<
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
		const mutation = captured.current as Mutation<
			HeldBulkReverifyResponse,
			{ held_ids?: number[] | null }
		>;

		mutation.onError(new Error('bulk nope'));

		expect(mockToast).toHaveBeenCalledWith({ message: 'bulk nope', type: 'error' });
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});
