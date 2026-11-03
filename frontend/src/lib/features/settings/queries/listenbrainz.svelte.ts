import type { components } from '$lib/api/v3/openapi';
import { ADMIN, key } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { createAppMutation } from '$lib/queries/effects';

export type ListenBrainzConnection = components['schemas']['ListenBrainzConnection'];
export type VerifyConnectionResponse = components['schemas']['VerifyConnectionResponse'];

// The instance's ListenBrainz connection, an admin setting.
export const ListenBrainzQueryKeyFactory = {
	connection: () => key(ADMIN, 'settings', 'listenbrainz')
};

const LISTENBRAINZ_TIMEOUT_MS = 10_000;

export const getListenBrainzConnectionQueryOptions = () =>
	queryOptions({
		queryKey: ListenBrainzQueryKeyFactory.connection(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/settings/listenbrainz'), {
				signal,
				timeoutMs: LISTENBRAINZ_TIMEOUT_MS
			})
	});

export const getListenBrainzConnectionQuery = () =>
	createQuery(() => ({
		...getListenBrainzConnectionQueryOptions(),
		enabled: authStore.isAdmin
	}));

// Save mutations refresh the cache and stay quiet: the settings tab shows
// inline receipts, which read better than toasts on a form.
export const createSaveListenBrainzMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: ListenBrainzConnection) =>
			api.v3.PUT(v3('/api/v3/settings/listenbrainz'), vars),
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
		mutationFn: (vars: ListenBrainzConnection) =>
			api.v3.POST(v3('/api/v3/settings/listenbrainz/verify'), vars)
	}));
