import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

import { api } from '$lib/api/client';
import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import { getPolicySummaryQuery } from './PolicyQueries.svelte';

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

describe('getPolicySummaryQuery', () => {
	it('reads the shared policy sentence behind the policy-summary key', async () => {
		mockGet.mockResolvedValue({ sentence: 'Everything allowed' });
		const query = getPolicySummaryQuery() as QueryResult;
		const controller = new AbortController();

		await query.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.policySummary(), {
			signal: controller.signal
		});
		expect(query.queryKey).toEqual(DownloadQueryKeyFactory.policySummary());
	});
});
