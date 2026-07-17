import { api } from '$lib/api/client';
import { API } from '$lib/constants';
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
	PlexPinResponse,
	SetupVars
} from './types';

/** Recovery redemption is public; code generation uses the authenticated admin client. */

export const createLocalLoginMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: LocalLoginVars) =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.login, vars)
	}));

export const createPasswordRecoveryResetMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: PasswordRecoveryResetVars) =>
			api.global.post<void>(API.auth.passwordRecoveryReset(), vars)
	}));

export const createPasswordRecoveryCodeMutation = () =>
	createMutation(() => ({
		mutationFn: (userId: string) =>
			api.post<PasswordRecoveryCodeResponse>(API.auth.adminPasswordRecovery(userId))
	}));

export const createJellyfinLoginMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: JellyfinLoginVars) =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.jellyfinLogin, vars)
	}));

export const createSetupMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: SetupVars) =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.setup, vars)
	}));

export const createOidcExchangeMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: OidcExchangeVars) =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.oidcExchange, vars)
	}));

export const createOidcAuthorizeMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.post<OidcAuthorizeResponse>(AUTH_ENDPOINTS.oidcAuthorize)
	}));

export const createPlexPinMutation = () =>
	createMutation(() => ({
		mutationFn: () => api.global.post<PlexPinResponse>(AUTH_ENDPOINTS.plexPin)
	}));
