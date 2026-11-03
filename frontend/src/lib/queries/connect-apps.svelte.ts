import { ConnectAppsQueryKeyFactory } from '$lib/queries/featureKeys';

import { v3, type V3QueryOf } from '$lib/api/v3/endpoint';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	appConnectSettingsChanged,
	appPasswordRosterChanged,
	appPasswordsChanged,
	connectAppsSettingsChanged,
	createAppMutation,
	myConnectAppsChanged
} from '$lib/queries/effects';
import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

// Connect Apps: the inbound OpenSubsonic and Jellyfin compatibility.
export type ConnectAppsSettings = Schemas['ConnectApps'];
export type AppConnectSettings = Schemas['AppConnect'];
export type ConnectRequest = Schemas['ConnectRequest'];
export type MyConnectApps = Schemas['MyConnectApps'];
export type AppPasswordView = Schemas['AppPasswordView'];
/** One active app-password in the admin roster; never carries a secret. */
export type AdminAppPasswordView = Schemas['AdminAppPasswordView'];

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

// The caller's own choices for their apps; keyed per user like app-passwords.
export const getMyConnectAppsQuery = () =>
	createQuery(() => ({
		enabled: !!authStore.user?.id,
		queryKey: ConnectAppsQueryKeyFactory.mine(authStore.user?.id ?? ''),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/me/connect-apps'), { signal })
	}));

export function createSaveMyConnectAppsMutation() {
	return createAppMutation(() => ({
		mutationFn: (choices: MyConnectApps) => api.v3.PUT(v3('/api/v3/me/connect-apps'), choices),
		effects: () => [myConnectAppsChanged(authStore.user?.id ?? '')]
	}));
}

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

// The apps that may sign in through the browser (app connect).
export const getAppConnectSettingsQuery = () =>
	createQuery(() => ({
		queryKey: ConnectAppsQueryKeyFactory.appConnect(),
		queryFn: ({ signal }) => api.v3.GET(v3('/api/v3/settings/app-connect'), { signal })
	}));

export function createSaveAppConnectSettingsMutation() {
	return createAppMutation(() => ({
		mutationFn: (settings: AppConnectSettings) =>
			api.v3.PUT(v3('/api/v3/settings/app-connect'), settings),
		effects: () => [appConnectSettingsChanged()]
	}));
}

/** What an app asks for at /connect, as its query. */
export type AppConnectQuery = V3QueryOf<'/api/v3/auth/app-connect/request'>;

/**
 * What a native app asks of the caller at /connect, read once: never retried,
 * refetched or kept, since a refusal is the answer and the request is good only
 * for now. `null` reads nothing.
 */
export const getAppConnectRequestQuery = (query: () => AppConnectQuery | null) =>
	createQuery(() => {
		const asked = query();
		return {
			queryKey: ConnectAppsQueryKeyFactory.appConnectRequest(authStore.user?.id, asked),
			enabled: asked !== null,
			retry: false,
			gcTime: 0,
			staleTime: Infinity,
			refetchOnWindowFocus: false,
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(v3('/api/v3/auth/app-connect/request', { query: asked ?? undefined }), {
					signal
				})
		};
	});

/** Allow the app: the answer is the address that hands the code back to it. */
export function createAuthorizeAppConnectMutation() {
	return createAppMutation(() => ({
		mutationFn: (request: ConnectRequest) =>
			api.v3.POST(v3('/api/v3/auth/app-connect/authorize'), request),
		effects: () => []
	}));
}
