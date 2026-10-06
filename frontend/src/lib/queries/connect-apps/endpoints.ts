import { v3 } from '$lib/api/v3/endpoint';

export const CONNECT_APPS_ENDPOINTS = {
	settings: () => v3('/api/v3/settings/connect-apps'),
	appPasswords: () => v3('/api/v3/me/app-passwords'),
	appPassword: (id: string) => v3('/api/v3/me/app-passwords/{id}', { path: { id } }),
	adminAppPasswords: () => v3('/api/v3/admin/app-passwords'),
	adminAppPassword: (id: string) => v3('/api/v3/admin/app-passwords/{id}', { path: { id } })
} as const;

/** The server refuses a new app-password once a user has this many active. */
export const APP_PASSWORD_CAP = 25;
