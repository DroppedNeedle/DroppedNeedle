import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn(), PUT: vi.fn() } }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { AUTH_ENDPOINTS } from './endpoints';
import { getUserQuotaQuery, saveUserQuota, userQuotaKey } from './UserQuotaQueries.svelte';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockPut = vi.mocked(api.v3.PUT) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: unknown;
	staleTime?: unknown;
};

type MutationResult = {
	mutationFn: (vars: { userId: string; override: Record<string, unknown> }) => Promise<unknown>;
	onSuccess?: (data: unknown, vars: { userId: string }) => unknown;
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
		expect(query.queryKey).toEqual(userQuotaKey('user-9'));
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
	it('writes the override block, then refreshes that user quota row', async () => {
		mockPut.mockResolvedValue({ quota_override: {} });
		const mutation = saveUserQuota() as unknown as MutationResult;
		const override = { max_requests: 25 };

		await mutation.mutationFn({ userId: 'user-9', override });

		expect(mockPut).toHaveBeenCalledWith(AUTH_ENDPOINTS.userQuota('user-9'), override);
		await mutation.onSuccess?.({}, { userId: 'user-9' });
		expect(mockInvalidate).toHaveBeenCalledWith({ queryKey: userQuotaKey('user-9') });
	});
});
