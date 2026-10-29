import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';

import { ConnectAppsQueryKeyFactory } from './ConnectAppsQueryKeyFactory';
import { CONNECT_APPS_ENDPOINTS } from './endpoints';

const settingsQueryOptions = () =>
	queryOptions({
		queryKey: ConnectAppsQueryKeyFactory.settings(),
		queryFn: ({ signal }) => api.v3.GET(CONNECT_APPS_ENDPOINTS.settings(), { signal })
	});

export const getConnectAppsSettingsQuery = () => createQuery(() => settingsQueryOptions());

// app-passwords are always the current user's own; the userId keys the cache per user.
// `enabled` gates the fetch until auth resolves so nothing is ever written under an
// empty-id key (matches the user-scoped query pattern elsewhere).
export const getAppPasswordsQuery = () =>
	createQuery(() => ({
		enabled: !!authStore.user?.id,
		queryKey: ConnectAppsQueryKeyFactory.appPasswords(authStore.user?.id ?? ''),
		queryFn: async ({ signal }) =>
			(await api.v3.GET(CONNECT_APPS_ENDPOINTS.appPasswords(), { signal })).app_passwords
	}));

// admin oversight: every user's active app-passwords (metadata only, no secrets)
export const getAdminAppPasswordsQuery = () =>
	createQuery(() => ({
		queryKey: ConnectAppsQueryKeyFactory.adminAppPasswords(),
		queryFn: async ({ signal }) =>
			(await api.v3.GET(CONNECT_APPS_ENDPOINTS.adminAppPasswords(), { signal })).app_passwords
	}));
