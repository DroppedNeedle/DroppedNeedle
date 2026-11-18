import type { components } from '$lib/api/v3/openapi';
import { ADMIN, key } from '$lib/queries/keys';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { createAppMutation } from '$lib/queries/effects';
import { refreshHomeLayouts } from '$lib/features/home/queries/layout.svelte';

export type Personalization = components['schemas']['Personalization'];

// Who decides the home layout, an admin setting.
export const PersonalizationQueryKeyFactory = {
	settings: () => key(ADMIN, 'settings', 'personalization')
};

export const getPersonalizationQueryOptions = () =>
	queryOptions({
		queryKey: PersonalizationQueryKeyFactory.settings(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/personalization'), { signal })
	});

export const getPersonalizationQuery = () =>
	createQuery(() => ({
		...getPersonalizationQueryOptions(),
		enabled: authStore.isAdmin
	}));

// The save refreshes every open home, since the posture changes what it serves.
export const createSavePersonalizationMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: Personalization) => api.v3.PUT(v3('/api/v3/settings/personalization'), vars),
		onSuccess: async (saved) => {
			await setQueryDataWithPersister(PersonalizationQueryKeyFactory.settings(), saved);
			await refreshHomeLayouts();
		}
	}));
