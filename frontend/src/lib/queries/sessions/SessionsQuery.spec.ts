import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), DELETE: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

vi.mock('../QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { AuthQueryKeyFactory } from '../auth/AuthQueryKeyFactory';
import { SessionsQueryKeyFactory } from './SessionsQueryKeyFactory';
import { SESSIONS_ENDPOINTS } from './endpoints';
import { getSessionsQuery, getSessionsQueryOptions } from './SessionsQuery.svelte';
import { createRevokeSessionMutation } from './SessionsMutations.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockDelete = vi.mocked(api.global.v3.DELETE) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: boolean;
};

type MutationResult = {
	mutationFn: (vars: unknown) => Promise<unknown>;
	onMutate?: (vars: unknown) => { userId: string | undefined };
	onSuccess?: (data: unknown, vars: unknown, context: { userId: string | undefined }) => unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('SessionsQueryKeyFactory', () => {
	it('nests under the shared auth prefix', () => {
		expect([...SessionsQueryKeyFactory.prefix]).toEqual([
			...AuthQueryKeyFactory.prefix,
			'sessions'
		]);
	});

	it('scopes the list key by userId', () => {
		expect(SessionsQueryKeyFactory.list('userA')).toEqual(['auth', 'sessions', 'list', 'userA']);
	});

	it('produces different keys for different users', () => {
		expect(SessionsQueryKeyFactory.list('userA')).not.toEqual(
			SessionsQueryKeyFactory.list('userB')
		);
	});

	it('normalizes a missing userId to null', () => {
		expect(SessionsQueryKeyFactory.list(undefined)).toEqual(['auth', 'sessions', 'list', null]);
	});
});

describe('getSessionsQuery', () => {
	it('reads the v3 sessions endpoint and forwards the abort signal', async () => {
		mockGet.mockResolvedValue({ sessions: [] });
		const options = getSessionsQueryOptions('userA') as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledTimes(1);
		expect(mockGet).toHaveBeenCalledWith(SESSIONS_ENDPOINTS.list(), {
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
	it('revokes by id then invalidates the sessions list', async () => {
		mockDelete.mockResolvedValue(undefined);
		const mutation = createRevokeSessionMutation() as unknown as MutationResult;
		const vars = { id: 'sess-9', label: 'Office laptop' };

		await mutation.mutationFn(vars);
		expect(mockDelete).toHaveBeenCalledWith(SESSIONS_ENDPOINTS.revoke('sess-9'));

		const context = mutation.onMutate?.(vars) ?? { userId: 'userA' };
		await mutation.onSuccess?.(undefined, vars, context);
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: SessionsQueryKeyFactory.list('userA')
		});
	});

	it('skips invalidation when the user changed mid-flight', async () => {
		const mutation = createRevokeSessionMutation() as unknown as MutationResult;
		const vars = { id: 'sess-9', label: 'Office laptop' };

		await mutation.onSuccess?.(undefined, vars, { userId: 'userB' });
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});
