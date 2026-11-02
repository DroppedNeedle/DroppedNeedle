import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
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
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/indexers'), { signal })
	});

export const getIndexersQuery = () => createQuery(() => getIndexersQueryOptions());

// Home reads integration_status; an added/removed indexer changes it.
const usenetChanged = () => [usenetSettingsChanged(), downloadReadinessChanged()];

export function createSaveIndexerMutation() {
	return createAppMutation(() => ({
		mutationFn: (indexer: IndexerSettings) =>
			indexer.id
				? api.v3.PUT(v3('/api/v3/settings/indexers/{id}', { path: { id: indexer.id } }), indexer)
				: api.v3.POST(v3('/api/v3/settings/indexers'), indexer),
		effects: usenetChanged
	}));
}

export function createDeleteIndexerMutation() {
	return createAppMutation(() => ({
		mutationFn: (id: string) =>
			api.v3.DELETE(v3('/api/v3/settings/indexers/{id}', { path: { id } })),
		effects: usenetChanged
	}));
}

export function createReorderIndexersMutation() {
	return createAppMutation(() => ({
		mutationFn: (orderedIds: string[]) =>
			api.v3.POST(v3('/api/v3/settings/indexers/reorder'), {
				ordered_ids: orderedIds
			}),
		effects: usenetChanged
	}));
}

export function createTestIndexerMutation() {
	return createAppMutation(() => ({
		effects: () => [],
		mutationFn: (indexer: IndexerSettings) =>
			api.v3.POST(v3('/api/v3/settings/indexers/test'), indexer)
	}));
}

const getSearchBackendQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.searchBackend(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/indexers/search-backend'), { signal })
	});

export const getSearchBackendQuery = () => createQuery(() => getSearchBackendQueryOptions());

export function createSaveSearchBackendMutation() {
	return createAppMutation(() => ({
		mutationFn: (backend: UsenetSearchBackendName) =>
			api.v3.PUT(v3('/api/v3/settings/indexers/search-backend'), {
				backend
			}),
		// Switching backends flips readiness, so status surfaces and Home
		// refresh alongside the selector.
		effects: usenetChanged
	}));
}
