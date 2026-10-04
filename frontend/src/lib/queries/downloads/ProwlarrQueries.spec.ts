import { beforeEach, describe, expect, it, vi } from 'vitest';

const captured = vi.hoisted(() => ({ current: null as Record<string, unknown> | null }));

vi.mock('@tanstack/svelte-query', () => ({
	createMutation: vi.fn((factory: () => Record<string, unknown>) => {
		captured.current = factory();
		return captured.current;
	}),
	createQuery: vi.fn(),
	queryOptions: vi.fn((opts: Record<string, unknown>) => opts)
}));
vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			v3: {
				PUT: vi.fn().mockResolvedValue({ success: true }),
				POST: vi.fn().mockResolvedValue({ valid: true, message: 'ok' })
			}
		}
	}
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { api } from '$lib/api/client';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import { DownloadQueryKeyFactory } from '$lib/queries/downloads/DownloadQueryKeyFactory';
import {
	saveProwlarrConfigMutation,
	testProwlarrMutation
} from '$lib/queries/downloads/ProwlarrQueries.svelte';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

const mockPut = vi.mocked(api.global.v3.PUT);
const mockPost = vi.mocked(api.global.v3.POST);
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

interface Mutation {
	mutationFn: (vars: unknown) => Promise<unknown>;
	onSuccess?: () => Promise<void> | void;
}

const connection = { enabled: true, url: 'http://prowlarr:9696', api_key: 'k' };

beforeEach(() => {
	vi.clearAllMocks();
});

describe('ProwlarrQueries', () => {
	it('saves via PUT config and sweeps the SABnzbd-shape invalidation set', async () => {
		saveProwlarrConfigMutation();
		const mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn(connection);
		expect(mockPut).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrConfig(), connection);
		await mutation.onSuccess?.();
		const swept = mockInvalidate.mock.calls.map((c) => c[0]?.queryKey);
		expect(swept).toContainEqual(DownloadQueryKeyFactory.prowlarr());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.sabnzbd());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.sabnzbdStatus());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.clientStatus());
		expect(swept).toContainEqual(HomeQueryKeyFactory.prefix);
	});

	it('tests via POST test without invalidating', async () => {
		testProwlarrMutation();
		const mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn(connection);
		expect(mockPost).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.prowlarrTest(), connection);
		expect(mutation.onSuccess).toBeUndefined();
		expect(mockInvalidate).not.toHaveBeenCalled();
	});

	it('nests the prowlarr key under the downloads prefix', () => {
		expect(DownloadQueryKeyFactory.prowlarr()).toEqual(['downloads', 'prowlarr']);
	});
});
