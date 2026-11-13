import type { components } from '$lib/api/v3/openapi';
import { ADMIN, key } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import {
	createAppMutation,
	discoverHomeChanged,
	homeChanged,
	recommendationSourcesChanged
} from '$lib/queries/effects';

export type ListenBrainzSettings = components['schemas']['ListenBrainzSettings'];

// The switch for the app's ListenBrainz reads, an admin setting.
export const ListenBrainzQueryKeyFactory = {
	settings: () => key(ADMIN, 'settings', 'listenbrainz')
};

const LISTENBRAINZ_TIMEOUT_MS = 10_000;

export const getListenBrainzSettingsQueryOptions = () =>
	queryOptions({
		queryKey: ListenBrainzQueryKeyFactory.settings(),
		queryFn: ({ signal }) =>
			api.v3.GET(v3('/api/v3/settings/listenbrainz'), {
				signal,
				timeoutMs: LISTENBRAINZ_TIMEOUT_MS
			})
	});

export const getListenBrainzSettingsQuery = () =>
	createQuery(() => ({
		...getListenBrainzSettingsQueryOptions(),
		enabled: authStore.isAdmin
	}));

// The save stays quiet (the tab shows an inline receipt) and refreshes every
// shelf that reads ListenBrainz, so switching it back on fills them at once.
export const createSaveListenBrainzMutation = () =>
	createAppMutation(() => ({
		effects: () => {
			const userId = authStore.user?.id;
			return [recommendationSourcesChanged(userId), discoverHomeChanged(userId), homeChanged()];
		},
		mutationFn: (vars: ListenBrainzSettings) =>
			api.v3.PUT(v3('/api/v3/settings/listenbrainz'), vars),
		onMutate: () => ({ admin: authStore.isAdmin }),
		onSuccess: async (saved, _vars, context) => {
			if (context.admin && authStore.isAdmin) {
				await setQueryDataWithPersister(ListenBrainzQueryKeyFactory.settings(), saved);
			}
		}
	}));
