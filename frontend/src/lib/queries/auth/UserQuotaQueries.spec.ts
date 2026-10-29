import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), PUT: vi.fn() } }
}));

// The real effects module loads the cache helpers; stub them.
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

vi.mock('$lib/queries/effects', async (importOriginal) => ({
	...(await importOriginal<typeof import('$lib/queries/effects')>()),
	createAppMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

import { api } from '$lib/api/client';
import { userQuotaChanged, type Effect } from '$lib/queries/effects';
import { AUTH_ENDPOINTS } from './endpoints';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { getUserQuotaQuery, saveUserQuota } from './UserQuotaQueries.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockPut = vi.mocked(api.v3.PUT) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: unknown;
	staleTime?: unknown;
};

type MutationResult = {
	mutationFn: (vars: { userId: string; override: Record<string, unknown> }) => Promise<unknown>;
	effects: (data: unknown, vars: { userId: string }) => Effect[];
};

beforeEach(() => {
	vi.clearAllMocks();
});

describe('getUserQuotaQuery', () => {
	it('reads the admin quota row for one user behind a per-user key', async () => {
		mockGet.mockResolvedValue({ quota_override: null });
		const query = getUserQuotaQuery(
			() => 'user-9',
			() => true
		) as QueryResult;
		const controller = new AbortController();

		await query.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(AUTH_ENDPOINTS.userQuota('user-9'), {
			signal: controller.signal
		});
		expect(query.queryKey).toEqual(AuthQueryKeyFactory.userQuota('user-9'));
	});

	it('stays disabled until the admin picker opens it', () => {
		const query = getUserQuotaQuery(
			() => 'user-9',
			() => false
		) as QueryResult;

		expect(query.enabled).toBe(false);
	});

	it('never serves stale usage counts from the persister', () => {
		const query = getUserQuotaQuery(
			() => 'user-9',
			() => true
		) as QueryResult;

		expect(query.staleTime).toBe(0);
	});
});

describe('saveUserQuota', () => {
	it('writes the override block and declares that user quota row changed', async () => {
		mockPut.mockResolvedValue({ quota_override: {} });
		const mutation = saveUserQuota() as unknown as MutationResult;
		const override = { max_requests: 25 };

		await mutation.mutationFn({ userId: 'user-9', override });

		expect(mockPut).toHaveBeenCalledWith(AUTH_ENDPOINTS.userQuota('user-9'), override);
		expect(mutation.effects({}, { userId: 'user-9' })).toEqual([userQuotaChanged('user-9')]);
	});
});
