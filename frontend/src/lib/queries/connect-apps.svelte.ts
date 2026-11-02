import { ConnectAppsQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3 } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	appPasswordRosterChanged,
	appPasswordsChanged,
	connectAppsSettingsChanged,
	createAppMutation
} from '$lib/queries/effects';
import type { ConnectAppsSettings } from '$lib/types';

/** The server refuses a new app-password once a user has this many active. */
export const APP_PASSWORD_CAP = 25;

const settingsQueryOptions = () =>
	queryOptions({
		queryKey: ConnectAppsQueryKeyFactory.settings(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/connect-apps'), { signal })
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
			(await api.v3.GET(v3('/api/v3/me/app-passwords'), { signal })).app_passwords
	}));

// admin oversight: every user's active app-passwords (metadata only, no secrets)
export const getAdminAppPasswordsQuery = () =>
	createQuery(() => ({
		queryKey: ConnectAppsQueryKeyFactory.adminAppPasswords(),
		queryFn: async ({ signal }) =>
			(await api.v3.GET(v3('/api/v3/admin/app-passwords'), { signal })).app_passwords
	}));

const ownAppPasswords = () => [appPasswordsChanged(authStore.user?.id ?? '')];

export function createSaveConnectAppsSettingsMutation() {
	return createAppMutation(() => ({
		mutationFn: (settings: ConnectAppsSettings) =>
			api.v3.PUT(v3('/api/v3/settings/connect-apps'), settings),
		effects: () => [connectAppsSettingsChanged()]
	}));
}

export function createAppPasswordMutation() {
	return createAppMutation(() => ({
		mutationFn: (name: string) => api.v3.POST(v3('/api/v3/me/app-passwords'), { name }),
		// returned secret stays in component state for the one-time reveal, never cached
		effects: ownAppPasswords
	}));
}

export function createRevokeAppPasswordMutation() {
	return createAppMutation(() => ({
		mutationFn: (id: string) =>
			api.v3.DELETE(v3('/api/v3/me/app-passwords/{id}', { path: { id } })),
		effects: ownAppPasswords
	}));
}

// admin revokes any user's app-password; pass the owning user_id so we can also
// refresh the admin's OWN profile list when they revoke one of their own (same
// browser). Another user's list can't be invalidated from here - their client
// refreshes on next load; server-side auth dies immediately regardless.
export function createAdminRevokeAppPasswordMutation() {
	return createAppMutation(() => ({
		mutationFn: (vars: { id: string; userId: string }) =>
			api.v3.DELETE(v3('/api/v3/admin/app-passwords/{id}', { path: { id: vars.id } })),
		effects: (_data: void, vars: { id: string; userId: string }) => [
			appPasswordRosterChanged(),
			...(vars.userId === authStore.user?.id ? ownAppPasswords() : [])
		]
	}));
}
