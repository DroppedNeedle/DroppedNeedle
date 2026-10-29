import { createMutation, createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { toastStore } from '$lib/stores/toast';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_TASKS_ENDPOINTS } from './endpoints';

const getQuarantineQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.quarantine(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_TASKS_ENDPOINTS.quarantine(), { signal })
	});

export const getQuarantineQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({ ...getQuarantineQueryOptions(), enabled: enabled() }));

export function deleteQuarantineEntry() {
	return createMutation(() => ({
		mutationFn: (id: number) => api.v3.DELETE(DOWNLOAD_TASKS_ENDPOINTS.quarantineDelete(id)),
		onSuccess: () => {
			toastStore.show({ message: 'Quarantine entry removed', type: 'success' });
			void invalidateQueriesWithPersister({ queryKey: DownloadQueryKeyFactory.quarantine() });
		},
		onError: (err: unknown) =>
			toastStore.show({
				message: err instanceof Error && err.message ? err.message : 'Failed to remove entry',
				type: 'error'
			})
	}));
}
