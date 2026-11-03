import { v3 } from '$lib/api/v3/endpoint';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), DELETE: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

// The real effects module loads the cache helpers; stub them.
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import {
	getSessionsQuery,
	getSessionsQueryOptions
} from '$lib/features/profile/queries/sessions.svelte';
import { createRevokeSessionMutation } from '$lib/features/profile/queries/sessions.svelte';
import { SessionsQueryKeyFactory } from '$lib/queries/featureKeys';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockDelete = vi.mocked(api.v3.DELETE) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: boolean;
};

type MutationResult = {
	mutationFn: (vars: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown) => { userId: string | undefined };
	onSuccess: (
		data: unknown,
		vars: unknown,
		context: { userId: string | undefined }
	) => Promise<void>;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('getSessionsQuery', () => {
	it('reads the v3 sessions endpoint and forwards the abort signal', async () => {
		mockGet.mockResolvedValue({ sessions: [] });
		const options = getSessionsQueryOptions('userA') as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledTimes(1);
		expect(mockGet).toHaveBeenCalledWith(v3('/api/v3/auth/sessions'), {
			signal: controller.signal,
			timeoutMs: expect.any(Number)
		});
	});

	it('is enabled only when a user is signed in', () => {
		const authed = getSessionsQuery() as QueryResult;
		expect(authed.enabled).toBe(true);

		setUser(null);
		const anon = getSessionsQuery() as QueryResult;
		expect(anon.enabled).toBe(false);
	});
});

describe('createRevokeSessionMutation', () => {
	it('revokes by id and declares the sessions list changed', async () => {
		mockDelete.mockResolvedValue(undefined);
		const mutation = createRevokeSessionMutation() as unknown as MutationResult;
		const vars = { id: 'sess-9', label: 'Office laptop' };

		await mutation.mutationFn(vars);
		expect(mockDelete).toHaveBeenCalledWith(
			v3('/api/v3/auth/sessions/{id}', { path: { id: 'sess-9' } })
		);

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess(undefined, vars, context);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: SessionsQueryKeyFactory.list('userA')
		});
	});

	it('declares nothing changed when the user changed mid-flight', async () => {
		const mutation = createRevokeSessionMutation() as unknown as MutationResult;
		const vars = { id: 'sess-9', label: 'Office laptop' };

		await mutation.onSuccess(undefined, vars, { userId: 'userB' });
		expect(invalidateQueriesWithPersister).not.toHaveBeenCalled();
	});
});
