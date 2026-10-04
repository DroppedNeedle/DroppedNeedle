import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { AUTH_ENDPOINTS } from './endpoints';
import type {
	AuthSessionResponse,
	JellyfinLoginVars,
	LocalLoginVars,
	OidcAuthorizeResponse,
	OidcExchangeVars,
	PasswordRecoveryCodeResponse,
	PasswordRecoveryResetVars,
	SetupVars
} from './types';

/** Recovery redemption is public; code generation uses the authenticated admin client. */

export const createLocalLoginMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: LocalLoginVars) => api.global.v3.POST(AUTH_ENDPOINTS.login, vars)
	}));

export const createPasswordRecoveryResetMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: PasswordRecoveryResetVars) =>
			api.global.v3.POST(AUTH_ENDPOINTS.passwordRecoveryReset, vars)
	}));

export const createPasswordRecoveryCodeMutation = () =>
	createMutation(() => ({
		mutationFn: (userId: string): Promise<PasswordRecoveryCodeResponse> =>
			api.v3.POST(AUTH_ENDPOINTS.adminPasswordRecovery(userId))
	}));

export const createJellyfinLoginMutation = () =>
	createMutation(() => ({
		// Untyped by necessity: the route answers the session user but its
		// spec entry carries no response schema, so the typed client would
		// infer void. Revisit once the backend annotates it.
		mutationFn: (vars: JellyfinLoginVars): Promise<AuthSessionResponse> =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.jellyfinLogin, vars)
	}));

export const createSetupMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: SetupVars) => api.global.v3.POST(AUTH_ENDPOINTS.setup, vars)
	}));

export const createOidcExchangeMutation = () =>
	createMutation(() => ({
		// Untyped by necessity: same missing response schema as the Jellyfin
		// login above.
		mutationFn: (vars: OidcExchangeVars): Promise<AuthSessionResponse> =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.oidcExchange, vars)
	}));

export const createOidcAuthorizeMutation = () =>
	createMutation(() => ({
		mutationFn: (): Promise<OidcAuthorizeResponse> =>
			api.global.v3.POST(AUTH_ENDPOINTS.oidcAuthorize)
	}));

// Plex sign-in rides the single v3 Plex flow ($lib/queries/plex): the login
// page mints pins via createPlexStartMutation, so no v1 pin mutation lives
// here anymore.
