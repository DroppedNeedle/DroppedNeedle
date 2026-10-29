import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { AUTH_ENDPOINTS } from './endpoints';
import type {
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
		mutationFn: (vars: LocalLoginVars) => api.v3.POST(AUTH_ENDPOINTS.login, vars)
	}));

export const createPasswordRecoveryResetMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: PasswordRecoveryResetVars) =>
			api.v3.POST(AUTH_ENDPOINTS.passwordRecoveryReset, vars)
	}));

export const createPasswordRecoveryCodeMutation = () =>
	createMutation(() => ({
		mutationFn: (userId: string): Promise<PasswordRecoveryCodeResponse> =>
			api.v3.POST(AUTH_ENDPOINTS.adminPasswordRecovery(userId))
	}));

export const createJellyfinLoginMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: JellyfinLoginVars) => api.v3.POST(AUTH_ENDPOINTS.jellyfinLogin, vars)
	}));

export const createSetupMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: SetupVars) => api.v3.POST(AUTH_ENDPOINTS.setup, vars)
	}));

export const createOidcExchangeMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: OidcExchangeVars) => api.v3.POST(AUTH_ENDPOINTS.oidcExchange, vars)
	}));

export const createOidcAuthorizeMutation = () =>
	createMutation(() => ({
		mutationFn: (): Promise<OidcAuthorizeResponse> => api.v3.POST(AUTH_ENDPOINTS.oidcAuthorize)
	}));

// Plex sign-in rides the single v3 Plex flow ($lib/queries/plex): the login
// page mints pins via createPlexStartMutation, so no v1 pin mutation lives
// here anymore.
