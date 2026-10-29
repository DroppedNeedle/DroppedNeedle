import { api } from '$lib/api/client';
import { createAppMutation } from '$lib/queries/effects';
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
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: LocalLoginVars) => api.v3.POST(AUTH_ENDPOINTS.login, vars)
	}));

export const createPasswordRecoveryResetMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: PasswordRecoveryResetVars) =>
			api.v3.POST(AUTH_ENDPOINTS.passwordRecoveryReset, vars)
	}));

export const createPasswordRecoveryCodeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (userId: string): Promise<PasswordRecoveryCodeResponse> =>
			api.v3.POST(AUTH_ENDPOINTS.adminPasswordRecovery(userId))
	}));

export const createJellyfinLoginMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: JellyfinLoginVars) => api.v3.POST(AUTH_ENDPOINTS.jellyfinLogin, vars)
	}));

export const createSetupMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: SetupVars) => api.v3.POST(AUTH_ENDPOINTS.setup, vars)
	}));

export const createOidcExchangeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (vars: OidcExchangeVars) => api.v3.POST(AUTH_ENDPOINTS.oidcExchange, vars)
	}));

export const createOidcAuthorizeMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (): Promise<OidcAuthorizeResponse> => api.v3.POST(AUTH_ENDPOINTS.oidcAuthorize)
	}));

// Plex sign-in rides the single v3 Plex flow ($lib/queries/plex): the login
// page mints pins via createPlexStartMutation, so no v1 pin mutation lives
// here anymore.
