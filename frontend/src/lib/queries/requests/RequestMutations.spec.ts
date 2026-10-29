import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { POST: vi.fn(), DELETE: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('$lib/stores/toast', () => ({
	toastStore: { show: vi.fn() }
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { albumStatusChanged, type Effect } from '../effects';
import { REQUESTS_ENDPOINTS } from './endpoints';
import {
	createApproveRequestMutation,
	createCancelRequestMutation,
	createRejectRequestMutation
} from './RequestMutations.svelte';

const mockPost = vi.mocked(api.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockDelete = vi.mocked(api.v3.DELETE) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

type MutationResult<Vars> = {
	mutationFn: (vars: Vars) => Promise<unknown>;
	onMutate?: (vars: Vars) => { userId: string | undefined };
	effects: (data: unknown, vars: Vars, context: { userId: string | undefined }) => Effect[];
	onError?: (err: unknown, vars: Vars) => unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

const names = (effects: Effect[]) => effects.map((effect) => effect.name);

// Every request action refreshes the requests, the downloads, the album it
// moved and the approvals count.
const SURFACE = [
	'anyRequestChanged',
	'anyDownloadChanged',
	'albumStatusChanged',
	'pendingApprovalsChanged'
];

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
		const effects = mutation.effects({ success: true }, vars, context);
		expect(names(effects)).toEqual(SURFACE);
		expect(effects).toContainEqual(albumStatusChanged('userA', 'mbid-1'));
	});

	it('declares nothing changed when the user changed mid-flight', () => {
		const mutation = createCancelRequestMutation() as unknown as MutationResult<{
			mbid: string;
			kind: 'album';
		}>;
		expect(
			mutation.effects({ success: true }, { mbid: 'mbid-1', kind: 'album' }, { userId: 'userB' })
		).toEqual([]);
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
		const effects = mutation.effects({ success: true }, vars, context);
		expect(names(effects)).toEqual(SURFACE);
		expect(effects).toContainEqual(albumStatusChanged('userA', 'mbid-3'));
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
		expect(names(mutation.effects({ success: true }, vars, context))).toEqual(SURFACE);
	});
});
