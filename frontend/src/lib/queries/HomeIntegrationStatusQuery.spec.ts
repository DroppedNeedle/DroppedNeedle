import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { get: vi.fn() } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-a' } as { id: string } | null },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { API } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import { getIntegrationStatusQuery } from './HomeIntegrationStatusQuery.svelte';

type QueryOptions = {
	queryKey: readonly unknown[];
	enabled: boolean;
	queryFn: (context: { signal: AbortSignal }) => Promise<unknown>;
};

describe('getIntegrationStatusQuery', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		(authStore as { user: { id: string } | null }).user = { id: 'user-a' };
	});

	it('uses a user-scoped key and forwards the abort signal', async () => {
		vi.mocked(api.global.get).mockResolvedValue({ download_client: true });
		const options = getIntegrationStatusQuery() as unknown as QueryOptions;
		const signal = new AbortController().signal;

		expect(options.queryKey).toEqual(['home', 'user-a', 'integration-status']);
		expect(options.enabled).toBe(true);
		await options.queryFn({ signal });
		expect(api.global.get).toHaveBeenCalledWith(API.homeIntegrationStatus(), { signal });
	});

	it('re-keys after a user switch', () => {
		(authStore as { user: { id: string } | null }).user = { id: 'user-b' };
		const options = getIntegrationStatusQuery() as unknown as QueryOptions;
		expect(options.queryKey).toEqual(['home', 'user-b', 'integration-status']);
	});

	it('stays disabled before a user is authenticated', () => {
		(authStore as { user: { id: string } | null }).user = null;
		const options = getIntegrationStatusQuery() as unknown as QueryOptions;

		expect(options.queryKey).toEqual(['home', null, 'integration-status']);
		expect(options.enabled).toBe(false);
	});
});
