import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { DropImportQueryKeyFactory } from './DropImportQueryKeyFactory';
import { DROP_IMPORT_ENDPOINTS } from './endpoints';
import type { DropImportJobList } from './types';

type Getter<T> = () => T;

// Curator-only surface (the route 403s anyone else). Polls while any job is
// still processing so per-item progress lands without a manual refresh; the
// drop_import_updated SSE event invalidates it for the terminal transitions.
export const getDropImportJobsQuery = (
	getEnabled: Getter<boolean>,
	getAll: Getter<boolean> = () => false
) =>
	createQuery(() => ({
		queryKey: DropImportQueryKeyFactory.jobs(authStore.user?.id, getAll()),
		// The server sends job and item status as plain strings; the screens
		// narrow them to the values it uses.
		queryFn: async ({ signal }) =>
			(await api.global.v3.GET(DROP_IMPORT_ENDPOINTS.jobs(getAll()), {
				signal
			})) as DropImportJobList,
		enabled: getEnabled(),
		refetchInterval: (query: { state: { data?: DropImportJobList } }) =>
			query.state.data?.jobs.some((job) => job.status === 'processing') ? 2000 : false
	}));
