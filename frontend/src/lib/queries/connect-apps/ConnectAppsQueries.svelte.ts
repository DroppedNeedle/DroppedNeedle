import { createQuery, queryOptions } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { authStore } from '$lib/stores/authStore.svelte';
import type { ConnectAppsSettings } from '$lib/types';

import { ConnectAppsQueryKeyFactory } from './ConnectAppsQueryKeyFactory';
import { CONNECT_APPS_ENDPOINTS } from './endpoints';

const settingsQueryOptions = () =>
	queryOptions({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: ConnectAppsQueryKeyFactory.settings(),
		// The contract types the enums as plain strings; the page model narrows them.
		queryFn: async ({ signal }) =>
			(await api.global.v3.GET(CONNECT_APPS_ENDPOINTS.settings(), {
				signal
			})) as ConnectAppsSettings
	});

export const getConnectAppsSettingsQuery = () => createQuery(() => settingsQueryOptions());

// app-passwords are always the current user's own; the userId keys the cache per user.
// `enabled` gates the fetch until auth resolves so nothing is ever written under an
// empty-id key (matches the user-scoped query pattern elsewhere).
export const getAppPasswordsQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		enabled: !!authStore.user?.id,
		queryKey: ConnectAppsQueryKeyFactory.appPasswords(authStore.user?.id ?? ''),
		queryFn: async ({ signal }) =>
			(await api.global.v3.GET(CONNECT_APPS_ENDPOINTS.appPasswords(), { signal })).app_passwords
	}));

// admin oversight: every user's active app-passwords (metadata only, no secrets)
export const getAdminAppPasswordsQuery = () =>
	createQuery(() => ({
		staleTime: CACHE_TTL.LIBRARY_NATIVE,
		queryKey: ConnectAppsQueryKeyFactory.adminAppPasswords(),
		queryFn: async ({ signal }) =>
			(await api.global.v3.GET(CONNECT_APPS_ENDPOINTS.adminAppPasswords(), { signal }))
				.app_passwords
	}));
