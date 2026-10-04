import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { POST: vi.fn(), DELETE: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

vi.mock('../QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { DownloadQueryKeyFactory } from '../downloads/DownloadQueryKeyFactory';
import { LibraryQueryKeyFactory } from '../library/LibraryQueryKeyFactory';
import { RequestQueryKeyFactory } from './RequestQueryKeyFactory';
import { REQUESTS_ENDPOINTS } from './endpoints';
import {
	createApproveRequestMutation,
	createBatchCancelRequestsMutation,
	createCancelRequestMutation,
	createClearHistoryMutation,
	createRejectRequestMutation,
	createRetryRequestMutation,
	createSyncRequestsMutation
} from './RequestMutations.svelte';

const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockDelete = vi.mocked(api.global.v3.DELETE) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

type MutationResult<Vars> = {
	mutationFn: (vars: Vars) => Promise<unknown>;
	onMutate?: (vars: Vars) => { userId: string | undefined };
	onSuccess?: (data: unknown, vars: Vars, context: { userId: string | undefined }) => unknown;
	onError?: (err: unknown, vars: Vars) => unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

function invalidatedPrefixes(): unknown[][] {
	return mockInvalidate.mock.calls.map((call) => [
		...((call[0] as { queryKey: readonly unknown[] }).queryKey ?? [])
	]);
}

describe('createCancelRequestMutation', () => {
	it('cancels by mbid, then sweeps requests, downloads, and the album key', async () => {
		mockDelete.mockResolvedValue({ success: true, message: 'Cancelled' });
		const mutation = createCancelRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		const vars = { mbid: 'mbid-1', kind: 'album' as const };

		await mutation.mutationFn(vars);
		expect(mockDelete).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.cancel('mbid-1', 'album'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({ success: true }, vars, context);
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RequestQueryKeyFactory.all]);
		expect(swept).toContainEqual([...DownloadQueryKeyFactory.all]);
		expect(swept).toContainEqual([...LibraryQueryKeyFactory.album('mbid-1')]);
	});

	it('skips invalidation when the user changed mid-flight', async () => {
		const mutation = createCancelRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		await mutation.onSuccess?.(
			{ success: true },
			{ mbid: 'mbid-1', kind: 'album' },
			{ userId: 'userB' }
		);
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});

describe('createRetryRequestMutation', () => {
	it('retries by mbid and sweeps requests plus downloads', async () => {
		mockPost.mockResolvedValue({ success: true, message: 'Retrying' });
		const mutation = createRetryRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		const vars = { mbid: 'mbid-2', kind: 'album' as const };

		await mutation.mutationFn(vars);
		expect(mockPost).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.retry('mbid-2', 'album'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({ success: true }, vars, context);
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RequestQueryKeyFactory.all]);
		expect(swept).toContainEqual([...DownloadQueryKeyFactory.all]);
		expect(swept).toContainEqual([...LibraryQueryKeyFactory.album('mbid-2')]);
	});
});

describe('createApproveRequestMutation', () => {
	it('approves by mbid and sweeps requests, downloads, and library', async () => {
		mockPost.mockResolvedValue({ success: true, message: 'Approved' });
		const mutation = createApproveRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		const vars = { mbid: 'mbid-3', kind: 'album' as const };

		await mutation.mutationFn(vars);
		expect(mockPost).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.approve('mbid-3', 'album'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({ success: true }, vars, context);
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RequestQueryKeyFactory.all]);
		expect(swept).toContainEqual([...DownloadQueryKeyFactory.all]);
		expect(swept).toContainEqual([...LibraryQueryKeyFactory.album('mbid-3')]);
	});
});

describe('createRejectRequestMutation', () => {
	it('rejects by mbid and sweeps the request surface', async () => {
		mockPost.mockResolvedValue({ success: true, message: 'Rejected' });
		const mutation = createRejectRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		const vars = { mbid: 'mbid-4', kind: 'album' as const };

		await mutation.mutationFn(vars);
		expect(mockPost).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.reject('mbid-4', 'album'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({ success: true }, vars, context);
		expect(invalidatedPrefixes()).toContainEqual([...RequestQueryKeyFactory.all]);
	});
});

describe('createBatchCancelRequestsMutation', () => {
	it('posts the R10 batch-cancel body and sweeps requests plus downloads', async () => {
		mockPost.mockResolvedValue({ success: true, cancelled: 2, failed: 0, message: 'Done' });
		const mutation = createBatchCancelRequestsMutation() as unknown as MutationResult<{
			mbids: string[];
			kind: 'album';
		}>;
		const vars = { mbids: ['a', 'b'], kind: 'album' as const };

		await mutation.mutationFn(vars);
		expect(mockPost).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.batchCancel(), {
			musicbrainz_ids: ['a', 'b'],
			kind: 'album'
		});

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({ success: true }, vars, context);
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RequestQueryKeyFactory.all]);
		expect(swept).toContainEqual([...DownloadQueryKeyFactory.all]);
		expect(swept).toContainEqual([...LibraryQueryKeyFactory.album('a')]);
		expect(swept).toContainEqual([...LibraryQueryKeyFactory.album('b')]);
	});
});

describe('createClearHistoryMutation', () => {
	it('clears one history row and sweeps the requests prefix', async () => {
		mockDelete.mockResolvedValue({ success: true });
		const mutation = createClearHistoryMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		const vars = { mbid: 'mbid-5', kind: 'album' as const };

		await mutation.mutationFn(vars);
		expect(mockDelete).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.clearHistory('mbid-5', 'album'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.({ success: true }, vars, context);
		expect(invalidatedPrefixes()).toContainEqual([...RequestQueryKeyFactory.all]);
	});
});

describe('createSyncRequestsMutation', () => {
	it('runs the status sync and sweeps requests plus downloads', async () => {
		mockPost.mockResolvedValue({ reconciled: 2 });
		const mutation = createSyncRequestsMutation() as unknown as MutationResult<void>;
		await mutation.mutationFn(undefined);
		expect(mockPost).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.sync());

		await mutation.onSuccess?.({ reconciled: 2 }, undefined, { userId: 'userA' });
		const swept = invalidatedPrefixes();
		expect(swept).toContainEqual([...RequestQueryKeyFactory.all]);
		expect(swept).toContainEqual([...DownloadQueryKeyFactory.all]);
	});
});
