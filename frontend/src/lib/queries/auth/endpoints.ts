import { v3 } from '$lib/api/v3/endpoint';

/** v3 auth endpoints, built through the typed registry. Login/setup happen
 * before a session exists, so those go through `api.global`, whose requests
 * survive page navigation; both clients share `createClient` and send the
 * session cookie alike, so this is about request lifetime, not credentials.
 * The `adminImport*` endpoints are post-session admin actions and use the
 * navigation-scoped `api` client instead. */
export const AUTH_ENDPOINTS = {
	providers: v3('/api/v3/auth/providers'),
	login: v3('/api/v3/auth/login'),
	jellyfinLogin: v3('/api/v3/auth/jellyfin/login'),
	setup: v3('/api/v3/auth/setup'),
	setupStatus: v3('/api/v3/auth/setup/status'),
	// The current-user read lives in the profile slice
	// (PROFILE_ENDPOINTS.get); it is not duplicated here.
	logout: v3('/api/v3/auth/logout'),
	oidcAuthorize: v3('/api/v3/auth/oidc/authorize'),
	oidcCallback: v3('/api/v3/auth/oidc/callback'),
	oidcExchange: v3('/api/v3/auth/oidc/exchange'),
	passwordRecoveryReset: v3('/api/v3/auth/password-recovery/reset'),
	adminUsers: (limit: number, offset: number) =>
		v3('/api/v3/admin/users', { query: { limit, offset } }),
	adminCreateUser: v3('/api/v3/admin/users'),
	adminUser: (userId: string) => v3('/api/v3/admin/users/{id}', { path: { id: userId } }),
	adminUserRole: (userId: string) => v3('/api/v3/admin/users/{id}/role', { path: { id: userId } }),
	adminPasswordRecovery: (userId: string) =>
		v3('/api/v3/admin/users/{id}/recovery-code', { path: { id: userId } }),
	adminImportJellyfin: v3('/api/v3/admin/import/jellyfin'),
	adminImportPlex: v3('/api/v3/admin/import/plex'),
	adminImport: v3('/api/v3/admin/import'),
	userQuota: (userId: string) => v3('/api/v3/admin/users/{id}/quota', { path: { id: userId } })
} as const;
