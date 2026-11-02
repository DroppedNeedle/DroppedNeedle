import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { toastStore } from '$lib/stores/toast';

import { DownloadQueryKeyFactory } from './DownloadQueryKeyFactory';
import { createAppMutation, quarantineChanged } from '$lib/queries/effects';

const getQuarantineQueryOptions = () =>
	queryOptions({
		queryKey: DownloadQueryKeyFactory.quarantine(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/downloads/quarantine'), { signal })
	});

export const getQuarantineQuery = (enabled: () => boolean = () => true) =>
	createQuery(() => ({ ...getQuarantineQueryOptions(), enabled: enabled() }));

export function deleteQuarantineEntry() {
	return createAppMutation(() => ({
		mutationFn: (id: number) =>
			api.v3.DELETE(
				v3('/api/v3/downloads/quarantine/{quarantine_id}', { path: { quarantine_id: id } })
			),
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
