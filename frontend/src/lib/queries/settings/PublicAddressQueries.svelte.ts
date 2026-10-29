import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { SettingsQueryKeyFactory } from './SettingsQueryKeyFactory';
import { SETTINGS_ENDPOINTS } from './endpoints';
import { createAppMutation } from '$lib/queries/effects';

export type PublicAddress = components['schemas']['PublicAddress'];

export const getPublicAddressQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: SettingsQueryKeyFactory.publicAddress(authStore.user?.id),
		queryFn: ({ signal }) => api.v3.GET(SETTINGS_ENDPOINTS.publicAddress(), { signal })
	}));

/** Replaces the list. The answer is the list as the server stored it. */
export const createSavePublicAddressMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (body: PublicAddress) => api.v3.PUT(SETTINGS_ENDPOINTS.publicAddress(), body),
		onSuccess: (saved: PublicAddress) =>
			setQueryDataWithPersister(SettingsQueryKeyFactory.publicAddress(authStore.user?.id), saved)
	}));
