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
				PUT: vi.fn().mockResolvedValue({ id: 'i1' }),
				POST: vi.fn().mockResolvedValue({ success: true }),
				DELETE: vi.fn().mockResolvedValue({ success: true })
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
	deleteIndexerMutation,
	reorderIndexersMutation,
	saveIndexerMutation,
	saveSearchBackendMutation,
	testIndexerMutation
} from '$lib/queries/downloads/IndexerQueries.svelte';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

const mockPut = vi.mocked(api.global.v3.PUT);
const mockPost = vi.mocked(api.global.v3.POST);
const mockDelete = vi.mocked(api.global.v3.DELETE);
const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);

interface Mutation {
	mutationFn: (vars: unknown) => Promise<unknown>;
	onSuccess?: () => Promise<void> | void;
}

const indexer = {
	id: 'i1',
	type: 'newznab',
	name: 'DS',
	url: 'https://idx.test/api',
	api_key: 'indexer****',
	categories: [3000],
	enabled: true,
	priority: 1
};

beforeEach(() => {
	vi.clearAllMocks();
});

function sweptKeys() {
	return mockInvalidate.mock.calls.map((c) => c[0]?.queryKey);
}

describe('IndexerQueries (B9 invalidation pin)', () => {
	it('saves via PUT update and sweeps indexers + client-status + Home', async () => {
		saveIndexerMutation();
		const mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn(indexer);
		expect(mockPut).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.indexer('i1'), indexer);
		await mutation.onSuccess?.();
		const swept = sweptKeys();
		expect(swept).toContainEqual(DownloadQueryKeyFactory.indexers());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.clientStatus());
		expect(swept).toContainEqual(HomeQueryKeyFactory.prefix);
	});

	it('deletes and reorders with the same sweep', async () => {
		deleteIndexerMutation();
		let mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn('i1');
		expect(mockDelete).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.indexer('i1'));
		await mutation.onSuccess?.();
		expect(sweptKeys()).toContainEqual(DownloadQueryKeyFactory.indexers());

		vi.clearAllMocks();
		reorderIndexersMutation();
		mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn(['i1']);
		expect(mockPost).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.indexersReorder(), { ordered_ids: ['i1'] });
		await mutation.onSuccess?.();
		expect(sweptKeys()).toContainEqual(HomeQueryKeyFactory.prefix);
	});

	it('tests via POST test without invalidating', async () => {
		testIndexerMutation();
		const mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn(indexer);
		expect(mockPost).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.indexersTest(), indexer);
		expect(mutation.onSuccess).toBeUndefined();
		expect(mockInvalidate).not.toHaveBeenCalled();
	});

	it('saves the search backend via PUT and sweeps the readiness set', async () => {
		saveSearchBackendMutation();
		const mutation = captured.current as unknown as Mutation;
		await mutation.mutationFn('prowlarr');
		expect(mockPut).toHaveBeenCalledWith(DOWNLOAD_SETTINGS_ENDPOINTS.searchBackend(), {
			backend: 'prowlarr'
		});
		await mutation.onSuccess?.();
		const swept = sweptKeys();
		expect(swept).toContainEqual(DownloadQueryKeyFactory.searchBackend());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.indexers());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.prowlarr());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.sabnzbd());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.sabnzbdStatus());
		expect(swept).toContainEqual(DownloadQueryKeyFactory.clientStatus());
		expect(swept).toContainEqual(HomeQueryKeyFactory.prefix);
	});

	it('nests the search-backend key under the indexers prefix', () => {
		expect(DownloadQueryKeyFactory.searchBackend()).toEqual([
			'downloads',
			'indexers',
			'search-backend'
		]);
	});
});
