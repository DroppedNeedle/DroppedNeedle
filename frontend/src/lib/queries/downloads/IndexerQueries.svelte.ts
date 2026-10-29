import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_SETTINGS_ENDPOINTS } from './endpoints';
import {
	createAppMutation,
	downloadReadinessChanged,
	usenetSettingsChanged
} from '$lib/queries/effects';

export type IndexerSettings = components['schemas']['NewznabIndexer'];
export type IndexerTestResult = components['schemas']['IndexerTestResponse'];
export type UsenetSearchBackendName = components['schemas']['UsenetBackend'];

const getIndexersQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.indexers(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.indexers(), { signal })
	});

export const getIndexersQuery = () => createQuery(() => getIndexersQueryOptions());

// Home reads integration_status; an added/removed indexer changes it.
const usenetChanged = () => [usenetSettingsChanged(), downloadReadinessChanged()];

export function saveIndexerMutation() {
	return createAppMutation(() => ({
		mutationFn: (indexer: IndexerSettings) =>
			indexer.id
				? api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.indexer(indexer.id), indexer)
				: api.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.indexers(), indexer),
		effects: usenetChanged
	}));
}

export function deleteIndexerMutation() {
	return createAppMutation(() => ({
		mutationFn: (id: string) => api.v3.DELETE(DOWNLOAD_SETTINGS_ENDPOINTS.indexer(id)),
		effects: usenetChanged
	}));
}

export function reorderIndexersMutation() {
	return createAppMutation(() => ({
		mutationFn: (orderedIds: string[]) =>
			api.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.indexersReorder(), {
				ordered_ids: orderedIds
			}),
		effects: usenetChanged
	}));
}

export function testIndexerMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (indexer: IndexerSettings) =>
			api.v3.POST(DOWNLOAD_SETTINGS_ENDPOINTS.indexersTest(), indexer)
	}));
}

const getSearchBackendQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.searchBackend(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_SETTINGS_ENDPOINTS.searchBackend(), { signal })
	});

export const getSearchBackendQuery = () => createQuery(() => getSearchBackendQueryOptions());

export function saveSearchBackendMutation() {
	return createAppMutation(() => ({
		mutationFn: (backend: UsenetSearchBackendName) =>
			api.v3.PUT(DOWNLOAD_SETTINGS_ENDPOINTS.searchBackend(), {
				backend
			}),
		// Switching backends flips readiness, so status surfaces and Home
		// refresh alongside the selector.
		effects: usenetChanged
	}));
}
