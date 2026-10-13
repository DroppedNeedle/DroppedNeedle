import { createMutation, createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { userIdSegment } from '../userKeySegment';
import { SETTINGS_ENDPOINTS } from './endpoints';

export type PublicAddress = components['schemas']['PublicAddress'];

export const PublicAddressQueryKeyFactory = {
	prefix: ['settings', 'public-address'] as const,
	saved: (userId: string | null | undefined) =>
		[...PublicAddressQueryKeyFactory.prefix, userIdSegment(userId)] as const
};

export const getPublicAddressQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: PublicAddressQueryKeyFactory.saved(authStore.user?.id),
		queryFn: ({ signal }) => api.global.v3.GET(SETTINGS_ENDPOINTS.publicAddress(), { signal })
	}));

/** Replaces the list. The answer is the list as the server stored it. */
export const createSavePublicAddressMutation = () =>
	createMutation(() => ({
		mutationFn: (body: PublicAddress) =>
			api.global.v3.PUT(SETTINGS_ENDPOINTS.publicAddress(), body),
		onSuccess: (saved: PublicAddress) =>
			setQueryDataWithPersister(PublicAddressQueryKeyFactory.saved(authStore.user?.id), saved)
	}));
