import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

import { api } from '$lib/api/client';
import { PROFILE_ENDPOINTS } from './endpoints';
import { getProfileQuery } from './ProfileQuery.svelte';
import { ProfileQueryKeyFactory } from './ProfileQueryKeyFactory';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

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

		expect(mockGet).toHaveBeenCalledWith(PROFILE_ENDPOINTS.get(), {
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
