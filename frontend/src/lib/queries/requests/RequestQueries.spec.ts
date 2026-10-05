import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory())
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn() } } }
}));

vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'userA' } as { id: string } | null, isAdmin: true },
	LAST_USER_ID_KEY: 'msr:last_user_id'
}));

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { RequestQueryKeyFactory } from './RequestQueryKeyFactory';
import { REQUESTS_ENDPOINTS } from './endpoints';
import {
	getActiveRequestCountQuery,
	getActiveRequestsQuery,
	getApprovalsQuery,
	getRequestHistoryQuery
} from './RequestQueries.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
	enabled?: unknown;
	refetchInterval?: unknown;
};

function setUser(user: { id: string } | null) {
	(authStore as { user: { id: string } | null }).user = user;
}

beforeEach(() => {
	vi.clearAllMocks();
	setUser({ id: 'userA' });
});

describe('getActiveRequestsQuery', () => {
	it('reads the v3 active endpoint and forwards the abort signal', async () => {
		mockGet.mockResolvedValue({ items: [], count: 0 });
		const options = getActiveRequestsQuery(() => true) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledTimes(1);
		expect(mockGet).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.active(), {
			signal: controller.signal
		});
		expect(options.queryKey).toEqual(RequestQueryKeyFactory.active('userA'));
	});

	it('stays disabled without a signed-in user', () => {
		setUser(null);
		const options = getActiveRequestsQuery(() => true) as QueryResult;
		expect(options.enabled).toBe(false);
	});

	it('polls the live list while the tab is visible', () => {
		const options = getActiveRequestsQuery(() => true) as QueryResult & {
			refetchIntervalInBackground?: unknown;
		};
		expect(options.refetchInterval).toBe(5_000);
		expect(options.refetchIntervalInBackground).toBe(false);
	});
});

describe('getActiveRequestCountQuery', () => {
	it('reads the cheap R10 count endpoint for badges', async () => {
		mockGet.mockResolvedValue({ count: 3 });
		const options = getActiveRequestCountQuery(() => true) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.activeCount(), {
			signal: controller.signal
		});
		expect(options.queryKey).toEqual(RequestQueryKeyFactory.activeCount('userA'));
	});
});

describe('getRequestHistoryQuery', () => {
	it('passes page, status, and sort through to the v3 history endpoint', async () => {
		mockGet.mockResolvedValue({ items: [], total: 0, page: 2, page_size: 20, total_pages: 1 });
		const options = getRequestHistoryQuery(
			() => ({ page: 2, status: 'failed', sort: 'oldest' }),
			() => true
		) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(
			REQUESTS_ENDPOINTS.history({ page: 2, status: 'failed', sort: 'oldest' }),
			{ signal: controller.signal }
		);
	});
});

describe('getApprovalsQuery', () => {
	it('reads the v3 approvals queue', async () => {
		mockGet.mockResolvedValue({ items: [], count: 0 });
		const options = getApprovalsQuery(() => true) as QueryResult;
		const controller = new AbortController();
		await options.queryFn?.({ signal: controller.signal });

		expect(mockGet).toHaveBeenCalledWith(REQUESTS_ENDPOINTS.approvals(), {
			signal: controller.signal
		});
		expect(options.queryKey).toEqual(RequestQueryKeyFactory.approvals('userA'));
	});
});
