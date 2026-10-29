import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '../QueryClient';
import { ListenBrainzQueryKeyFactory } from './ListenBrainzQueryKeyFactory';
import { LISTENBRAINZ_ENDPOINTS } from './endpoints';
import type { ListenBrainzConnection } from './types';
import { createAppMutation } from '$lib/queries/effects';

// Save mutations refresh the cache and stay quiet: the settings tab shows
// inline receipts, which read better than toasts on a form.
export const createSaveListenBrainzMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: ListenBrainzConnection) =>
			api.v3.PUT(LISTENBRAINZ_ENDPOINTS.connection, vars),
		onMutate: () => ({ admin: authStore.isAdmin }),
		onSuccess: async (saved, _vars, context) => {
			if (context.admin && authStore.isAdmin) {
				await setQueryDataWithPersister(ListenBrainzQueryKeyFactory.connection(), saved);
			}
		}
	}));

// Verify only probes the submitted values; the tab renders the verdict.
export const createVerifyListenBrainzMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: ListenBrainzConnection) => api.v3.POST(LISTENBRAINZ_ENDPOINTS.verify, vars)
	}));
