import { api } from '$lib/api/client';
import {
	appPasswordRosterChanged,
	appPasswordsChanged,
	connectAppsSettingsChanged,
	createAppMutation
} from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import type { ConnectAppsSettings } from '$lib/types';

import { CONNECT_APPS_ENDPOINTS } from './endpoints';

const ownAppPasswords = () => [appPasswordsChanged(authStore.user?.id ?? '')];

export function saveConnectAppsSettings() {
	return createAppMutation(() => ({
		mutationFn: (settings: ConnectAppsSettings) =>
			api.v3.PUT(CONNECT_APPS_ENDPOINTS.settings(), settings),
		effects: () => [connectAppsSettingsChanged()]
	}));
}

export function createAppPassword() {
	return createAppMutation(() => ({
		mutationFn: (name: string) => api.v3.POST(CONNECT_APPS_ENDPOINTS.appPasswords(), { name }),
		// returned secret stays in component state for the one-time reveal, never cached
		effects: ownAppPasswords
	}));
}

export function revokeAppPassword() {
	return createAppMutation(() => ({
		mutationFn: (id: string) => api.v3.DELETE(CONNECT_APPS_ENDPOINTS.appPassword(id)),
		effects: ownAppPasswords
	}));
}

// admin revokes any user's app-password; pass the owning user_id so we can also
// refresh the admin's OWN profile list when they revoke one of their own (same
// browser). Another user's list can't be invalidated from here - their client
// refreshes on next load; server-side auth dies immediately regardless.
export function adminRevokeAppPassword() {
	return createAppMutation(() => ({
		mutationFn: (vars: { id: string; userId: string }) =>
			api.v3.DELETE(CONNECT_APPS_ENDPOINTS.adminAppPassword(vars.id)),
		effects: (_data: void, vars: { id: string; userId: string }) => [
			appPasswordRosterChanged(),
			...(vars.userId === authStore.user?.id ? ownAppPasswords() : [])
		]
	}));
}
