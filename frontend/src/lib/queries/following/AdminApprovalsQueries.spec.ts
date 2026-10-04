import { describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => unknown) => factory())
}));

const mockGet = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: (...args: unknown[]) => mockGet(...args) } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'admin-1' } }
}));

import { getPendingApprovalCountQuery } from './AdminApprovalsQueries.svelte';

describe('pending approval count query', () => {
	it('is admin-user keyed and shares the one-request-per-minute idle budget', async () => {
		const options = getPendingApprovalCountQuery(() => true) as unknown as {
			queryKey: readonly unknown[];
			queryFn: (context: { signal?: AbortSignal }) => unknown;
			refetchInterval: number;
			refetchIntervalInBackground: boolean;
			refetchOnReconnect: string;
			refetchOnWindowFocus: string;
		};

		await options.queryFn({ signal: undefined });

		expect(options.queryKey).toEqual(['following', 'admin-approvals', 'count', 'admin-1']);
		expect(mockGet.mock.calls.at(-1)?.[0]).toBe('/api/v3/requests/approvals/count');
		expect(options.refetchInterval).toBe(120_000);
		expect(options.refetchIntervalInBackground).toBe(false);
		expect(options.refetchOnReconnect).toBe('always');
		expect(options.refetchOnWindowFocus).toBe('always');
	});
});
