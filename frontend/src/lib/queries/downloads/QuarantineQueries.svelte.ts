import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { DOWNLOAD_TASKS_ENDPOINTS } from './endpoints';
import { createAppMutation, quarantineChanged } from '$lib/queries/effects';

const getQuarantineQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.quarantine(),
		queryFn: ({ signal }) => api.v3.GET(DOWNLOAD_TASKS_ENDPOINTS.quarantine(), { signal })
	});

export const getQuarantineQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({ ...getQuarantineQueryOptions(), enabled: enabled() }));

export function deleteQuarantineEntry() {
	return createAppMutation(() => ({
		mutationFn: (id: number) => api.v3.DELETE(DOWNLOAD_TASKS_ENDPOINTS.quarantineDelete(id)),
		onSuccess: () => {
			toastStore.show({ message: 'Quarantine entry removed', type: 'success' });
		},
		effects: () => [quarantineChanged()],
		onError: (err: unknown) =>
			toastStore.show({
				message: err instanceof Error && err.message ? err.message : 'Failed to remove entry',
				type: 'error'
			})
	}));
}
