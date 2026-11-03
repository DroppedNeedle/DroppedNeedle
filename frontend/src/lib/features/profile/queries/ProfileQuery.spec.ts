import { v3 } from '$lib/api/v3/endpoint';
import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: vi.fn() } }
}));

// The real effects module loads the cache helpers; stub them.
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn(),
	setQueryDataWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { getProfileQuery } from '$lib/features/profile/queries/profile.svelte';
import { ProfileQueryKeyFactory } from '$lib/queries/featureKeys';

const mockGet = vi.mocked(api.v3.GET) as unknown as Mock<(...args: unknown[]) => Promise<unknown>>;

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
};

beforeEach(() => {
	vi.clearAllMocks();
});

describe('getProfileQuery', () => {
	it('reads the caller identity behind a per-user key', async () => {
		mockGet.mockResolvedValue({ id: 'user-1' });
		const query = getProfileQuery(() => 'user-1') as QueryResult;
		const controller = new AbortController();

		await query.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(v3('/api/v3/me'), {
			signal: controller.signal
		});
		expect(query.queryKey).toEqual(ProfileQueryKeyFactory.profile('user-1'));
	});

	it('keys each user separately so shared browsers never leak identity', () => {
		const first = getProfileQuery(() => 'user-1') as QueryResult;
		const second = getProfileQuery(() => 'user-2') as QueryResult;

		expect(first.queryKey).not.toEqual(second.queryKey);
	});
});
