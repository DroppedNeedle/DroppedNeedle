import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	})
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), POST: vi.fn(), PUT: vi.fn(), DELETE: vi.fn() } } }
}));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('./LibraryCatalogInvalidation', () => ({
	invalidateLibraryCatalog: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';
import { invalidateLibraryCatalog } from './LibraryCatalogInvalidation';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import {
	addLibraryRootV3,
	applyLibraryManageV3,
	approveLibraryReviewV3,
	clearLibraryEditionPinV3,
	enqueueLibraryIdentifyV3,
	previewLibraryManageV3,
	rejectLibraryReviewV3,
	restoreLibraryBaselineV3,
	setLibraryEditionPinV3,
	triggerLibraryScanV3,
	undoLibraryManageV3
} from './LibraryV3Mutations.svelte';

type Mutation<TVars, TData = unknown> = {
	mutationFn: (vars: TVars) => Promise<TData>;
	onSuccess?: (data: TData, vars: TVars) => Promise<unknown> | unknown;
	onError?: (error: unknown) => unknown;
};

const mutation = <TVars, TData = unknown>() =>
	captured.current as unknown as Mutation<TVars, TData>;

beforeEach(() => vi.clearAllMocks());

describe('LibraryV3Mutations', () => {
	it('pins an edition and refreshes the pin plus album detail', async () => {
		setLibraryEditionPinV3();
		const vars = { userId: 'user-1', albumId: 'album-1', releaseMbid: 'rel-1' };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.PUT).toHaveBeenCalledWith('/api/v3/library/albums/album-1/edition-pin', {
			release_mbid: 'rel-1'
		});
		await mutation<typeof vars>().onSuccess?.({}, vars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.editionPin('user-1', 'album-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.albumDetail('user-1', 'album-1')
		});
	});

	it('refuses to pin without a local album id', () => {
		clearLibraryEditionPinV3();
		expect(() =>
			mutation<{ userId: string; albumId: string }>().mutationFn({
				userId: 'user-1',
				albumId: ''
			})
		).toThrow('Missing local album id');
		expect(api.global.v3.DELETE).not.toHaveBeenCalled();
	});

	it('clears an edition pin through DELETE', async () => {
		clearLibraryEditionPinV3();
		const vars = { userId: 'user-1', albumId: 'album-1' };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.DELETE).toHaveBeenCalledWith('/api/v3/library/albums/album-1/edition-pin');
		await mutation<typeof vars>().onSuccess?.({}, vars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.editionPin('user-1', 'album-1')
		});
	});

	it('enqueues identification and sweeps the catalog cross-domain', async () => {
		enqueueLibraryIdentifyV3();
		const vars = { album_id: 'album-1', kind: 'manual' };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/identify', vars);
		await mutation<typeof vars>().onSuccess?.({} as never, vars);
		expect(invalidateLibraryCatalog).toHaveBeenCalledOnce();
		expect(toastStore.show).toHaveBeenCalledWith({
			message: 'Identification started',
			type: 'success'
		});
	});

	it('previews management without touching the cache', async () => {
		previewLibraryManageV3();
		const vars = { album_id: 'album-1', kind: 'retag', items: [] };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/manage/preview', vars);
		expect(mutation<typeof vars>().onSuccess).toBeUndefined();
		expect(invalidateLibraryCatalog).not.toHaveBeenCalled();
	});

	it('publishes, undoes, and restores through the full catalog sweep', async () => {
		applyLibraryManageV3();
		await mutation<{ preview_token: string }>().mutationFn({ preview_token: 'tok' });
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/manage/apply', {
			preview_token: 'tok'
		});
		await mutation<{ preview_token: string }>().onSuccess?.({} as never, {
			preview_token: 'tok'
		});
		expect(invalidateLibraryCatalog).toHaveBeenCalledTimes(1);

		undoLibraryManageV3();
		await mutation<{ bundle_id: string }>().mutationFn({ bundle_id: 'b-1' });
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/manage/undo', {
			bundle_id: 'b-1'
		});
		await mutation<{ bundle_id: string }>().onSuccess?.({} as never, { bundle_id: 'b-1' });
		expect(invalidateLibraryCatalog).toHaveBeenCalledTimes(2);

		restoreLibraryBaselineV3();
		await mutation<{ track_ids: string[] }>().mutationFn({ track_ids: ['t-1'] });
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/manage/baseline/restore', {
			track_ids: ['t-1']
		});
		await mutation<{ track_ids: string[] }>().onSuccess?.({} as never, {
			track_ids: ['t-1']
		});
		expect(invalidateLibraryCatalog).toHaveBeenCalledTimes(3);
	});

	it('settles reviews through the full catalog sweep', async () => {
		approveLibraryReviewV3();
		const approveVars = { reviewId: 'r-1', candidateKey: 'cand-1' };
		await mutation<typeof approveVars>().mutationFn(approveVars);
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/reviews/r-1/approve', {
			candidate_key: 'cand-1'
		});
		await mutation<typeof approveVars>().onSuccess?.({} as never, approveVars);
		expect(invalidateLibraryCatalog).toHaveBeenCalledTimes(1);

		rejectLibraryReviewV3();
		await mutation<{ reviewId: string }>().mutationFn({ reviewId: 'r-2' });
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/reviews/r-2/reject');
		await mutation<{ reviewId: string }>().onSuccess?.({} as never, { reviewId: 'r-2' });
		expect(invalidateLibraryCatalog).toHaveBeenCalledTimes(2);
	});

	it('triggers a scan and refreshes the runs plus the new run', async () => {
		triggerLibraryScanV3();
		await mutation<{ root_id?: string }>().mutationFn({});
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/scan', {});
		await mutation<{ root_id?: string }>().onSuccess?.(
			{ run_id: 'run-9', disposition: 'started', state: 'queued' } as never,
			{}
		);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.scanRuns('user-1')
		});
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.scanRun('user-1', 'run-9')
		});
	});

	it('adds a root and refreshes the root registry', async () => {
		addLibraryRootV3();
		const vars = { path: '/music' };
		await mutation<typeof vars>().mutationFn(vars);
		expect(api.global.v3.POST).toHaveBeenCalledWith('/api/v3/library/roots', vars);
		await mutation<typeof vars>().onSuccess?.({} as never, vars);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: LibraryQueryKeyFactory.v3.roots('user-1')
		});
	});
});
