import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { CACHE_TTL } from '$lib/constants';
import { HomeQueryKeyFactory } from '$lib/queries/HomeQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';

export type IndexerSettings = components['schemas']['NewznabIndexerDto'];
export type IndexerSavedResponse = components['schemas']['IndexerSavedResponse'];
export type IndexerTestResult = components['schemas']['IndexerTestResponse'];
export type OperationResult = components['schemas']['OperationResult'];
export type UsenetSearchBackend = components['schemas']['UsenetSearchBackendDto'];
export type UsenetSearchBackendName = components['schemas']['UsenetBackendDto'];

const getIndexersQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.indexers(),
		queryFn: ({ signal }) => api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.indexers(), { signal })
	});

export const getIndexersQuery = () => createQuery(() => getIndexersQueryOptions());

async function invalidateIndexers() {
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.indexers() });
	await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.clientStatus() });
	// Home reads integration_status; an added/removed indexer changes it.
	await invalidateQueriesWithPersister({ queryKey: HomeQueryKeyFactory.prefix });
}

export function saveIndexerMutation() {
	return createMutation(() => ({
		mutationFn: (indexer: IndexerSettings) =>
			indexer.id
				? api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.indexer(indexer.id), indexer)
				: api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.indexers(), indexer),
		onSuccess: invalidateIndexers
	}));
}

export function deleteIndexerMutation() {
	return createMutation(() => ({
		mutationFn: (id: string) => api.global.v3.DELETE(DOWNLOAD_SETTINGS_ENDPOINTS.indexer(id)),
		onSuccess: invalidateIndexers
	}));
}

export function reorderIndexersMutation() {
	return createMutation(() => ({
		mutationFn: (orderedIds: string[]) =>
			api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.indexersReorder(), {
				ordered_ids: orderedIds
			}),
		onSuccess: invalidateIndexers
	}));
}

export function testIndexerMutation() {
	return createMutation(() => ({
		mutationFn: (indexer: IndexerSettings) =>
			api.global.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.indexersTest(), indexer)
	}));
}

const getSearchBackendQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: DownloadQueryKeyFactory.searchBackend(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.searchBackend(), { signal })
	});

export const getSearchBackendQuery = () => createQuery(() => getSearchBackendQueryOptions());

export function saveSearchBackendMutation() {
	return createMutation(() => ({
		mutationFn: (backend: UsenetSearchBackendName) =>
			api.global.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.searchBackend(), {
				backend
			}),
		onSuccess: async () => {
			// Switching backends flips readiness: sweep the SABnzbd-shape set so
			// status surfaces and Home refresh alongside the selector. The own key
			// is listed explicitly (it nests under indexers(), so the prefix sweep
			// below already covers it - this survives a future key move).
			await invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.searchBackend()
			});
			await invalidateIndexers();
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.prowlarr() });
			await invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.sabnzbd() });
			await invalidateQueriesWithPersister({
				queryKey: DownloadQueryKeyFactory.sabnzbdStatus()
			});
		}
	}));
}
