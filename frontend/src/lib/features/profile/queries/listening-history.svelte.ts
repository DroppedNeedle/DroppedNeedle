import { createQuery } from '@tanstack/svelte-query';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { createAppMutation, listeningHistoryChanged } from '$lib/queries/effects';
import { ListeningHistoryQueryKeyFactory } from '$lib/queries/featureKeys';

export type ListeningHistory = components['schemas']['ListeningHistory'];

// While a file is being read, the count moves: ask again every few seconds.
const READING_REFETCH_MS = 3000;
// While ListenBrainz makes the export, which takes minutes: ask less often.
const EXPORTING_REFETCH_MS = 15000;

export const getListeningHistoryQuery = () =>
	createQuery(() => ({
		queryKey: ListeningHistoryQueryKeyFactory.get(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/me/listening-history'), { signal }),
		enabled: Boolean(authStore.user?.id),
		refetchInterval: (query) => {
			const state = query.state.data?.state;
			if (state === 'importing') return READING_REFETCH_MS;
			if (state === 'pending' || state === 'exporting') return EXPORTING_REFETCH_MS;
			return false;
		}
	}));

// The file is read in the background; the status query follows it.
export const createUploadListeningHistoryMutation = () =>
	createAppMutation(() => ({
		effects: () => [listeningHistoryChanged(authStore.user?.id)],
		mutationFn: (file: File) => {
			const form = new FormData();
			form.append('file', file, file.name);
			return api.upload<ListeningHistory>(v3('/api/v3/me/listening-history/upload'), form);
		}
	}));

// After a forget or a failure: the server asks ListenBrainz for an export again.
export const createImportListeningHistoryMutation = () =>
	createAppMutation(() => ({
		effects: () => [listeningHistoryChanged(authStore.user?.id)],
		mutationFn: () => api.v3.POST(v3('/api/v3/me/listening-history/import'))
	}));
