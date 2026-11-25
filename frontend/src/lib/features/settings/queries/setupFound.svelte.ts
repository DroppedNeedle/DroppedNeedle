import { createQuery } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { v3 } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { ADMIN, key } from '$lib/queries/keys';
import { authStore } from '$lib/stores/authStore.svelte';

export type SetupFound = components['schemas']['SetupFound'];
export type FoundService = components['schemas']['FoundService'];
export type FoundFolder = components['schemas']['FoundFolder'];
export type SetupServiceKind = components['schemas']['SetupServiceKind'];
export type FolderLookState = components['schemas']['FolderLookState'];

export const SetupQueryKeyFactory = {
	found: () => key(ADMIN, 'settings', 'setup-found')
};

/**
 * What the setup guide found: the services answering on the server's network
 * and the mounted folders worth suggesting. Admin only. The look takes a few
 * seconds, so one answer serves the guide's visits for a while.
 */
export const getSetupFoundQuery = () =>
	createQuery(() => ({
		enabled: authStore.isAdmin,
		queryKey: SetupQueryKeyFactory.found(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/setup/found'), { signal }),
		staleTime: 5 * 60 * 1000
	}));
