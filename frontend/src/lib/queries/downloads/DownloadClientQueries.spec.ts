import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';

vi.mock('@tanstack/svelte-query', () => ({
	createQuery: vi.fn((factory: () => Record<string, unknown>) => factory()),
	createMutation: vi.fn((factory: () => Record<string, unknown>) => factory()),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: vi.fn(), PUT: vi.fn(), POST: vi.fn() } } }
}));

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn()
}));

import { api } from '$lib/api/client';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import {
	getDownloadClientConfigQuery,
	getDownloadClientStatusQuery,
	saveDownloadClientConfig,
	testDownloadClient,
	type DownloadClientConfig
} from './DownloadClientQueries.svelte';

const mockGet = vi.mocked(api.global.v3.GET) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPut = vi.mocked(api.global.v3.PUT) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockPost = vi.mocked(api.global.v3.POST) as unknown as Mock<
	(...args: unknown[]) => Promise<unknown>
>;
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

type QueryResult = {
	queryKey?: unknown;
	queryFn?: (ctx: { signal: AbortSignal }) => Promise<unknown>;
};

beforeEach(() => {
	vi.clearAllMocks();
});

describe('getDownloadClientConfigQuery', () => {
	it('reads the slskd connection form behind the client-config key', async () => {
		mockGet.mockResolvedValue({ host: 'slskd' });
		const query = getDownloadClientConfigQuery() as QueryResult;

		await query.queryFn?.({ signal: new AbortController().signal });

		expect(mockGet).toHaveBeenCalledWith(
			DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(),
			expect.objectContaining({})
		);
		expect(query.queryKey).toEqual(DownloadQueryKeyFactory.clientConfig());
	});
});

describe('getDownloadClientStatusQuery', () => {
	it('maps a reachable daemon onto the ok client view', async () => {
		mockGet.mockResolvedValue({
			configured: true,
			reachable: true,
			version: '1.2.3',
			message: 'Connected'
		});
		const query = getDownloadClientStatusQuery() as QueryResult;

		const status = (await query.queryFn?.({ signal: new AbortController().signal })) as {
			client: { status: string };
			mount?: unknown;
		};

		expect(status.client.status).toBe('ok');
		expect(query.queryKey).toEqual(DownloadQueryKeyFactory.clientStatus());
	});

	it('maps an unreachable daemon onto the error client view with no mount half', async () => {
		mockGet.mockResolvedValue({
			configured: true,
			reachable: false,
			version: null,
			message: 'Refused'
		});
		const query = getDownloadClientStatusQuery() as QueryResult;

		const status = (await query.queryFn?.({ signal: new AbortController().signal })) as {
			client: { status: string; message: string };
			mount?: unknown;
		};

		expect(status.client.status).toBe('error');
		expect(status.client.message).toBe('Refused');
		expect(status.mount).toBeUndefined();
	});
});

describe('saveDownloadClientConfig', () => {
	it('writes the config, then refreshes config, status, and the home prompt', async () => {
		const config = { host: 'slskd' } as unknown as DownloadClientConfig;
		mockPut.mockResolvedValue(config);
		const mutation = saveDownloadClientConfig() as unknown as {
			mutationFn: (config: DownloadClientConfig) => Promise<unknown>;
			onSuccess?: () => Promise<void>;
		};

		await mutation.mutationFn(config);

		expect(mockPut).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.slskdConfig(), config);
		await mutation.onSuccess?.();
		const keys = mockInvalidate.mock.calls.map(
			(call) => (call[0] as { queryKey: unknown }).queryKey
		);
		expect(keys).toContainEqual(DownloadQueryKeyFactory.clientConfig());
		expect(keys).toContainEqual(DownloadQueryKeyFactory.clientStatus());
		expect(keys).toContainEqual(HomeQueryKeyFactory.prefix);
	});
});

describe('testDownloadClient', () => {
	it('probes the candidate config without touching cached reads', async () => {
		const config = { host: 'candidate' } as unknown as DownloadClientConfig;
		mockPost.mockResolvedValue({ success: true });
		const mutation = testDownloadClient() as unknown as {
			mutationFn: (config: DownloadClientConfig) => Promise<unknown>;
			onSuccess?: () => Promise<void>;
		};

		await mutation.mutationFn(config);

		expect(mockPost).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.slskdTest(), config);
		expect(mutation.onSuccess).toBeUndefined();
		expect(mockInvalidate).not.toHaveBeenCalled();
	});
});
