import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { toastStore } from '$lib/stores/toast';
import { createAppMutation, discoverHistoryForgotten } from '$lib/queries/effects';

// Removes the picks Discover showed and how plays ended; the server builds
// the Discover page again, so every answer built from them is dropped.
export const createForgetDiscoverHistoryMutation = () =>
	createAppMutation(() => ({
		effects: () => [discoverHistoryForgotten(authStore.user?.id)],
		mutationFn: () => api.v3.DELETE(v3('/api/v3/discover/history')),
		onSuccess: () => toastStore.show({ message: 'History forgotten.', type: 'success' })
	}));
