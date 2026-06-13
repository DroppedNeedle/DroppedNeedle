import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { AUTH_ENDPOINTS } from './endpoints';
import type {
	AuthSessionResponse,
	JellyfinLoginVars,
	LocalLoginVars,
	OidcAuthorizeResponse,
	OidcExchangeVars,
	PlexPinResponse,
	SetupVars
} from './types';

/**
 * Auth mutations. Each routes through the unauthenticated `api.global` client, so
 * a non-2xx response rejects with an `ApiError` the caller surfaces via `.message`.
 * Mutations are not cached, so no queryKey/staleTime is needed.
 */

export const createLocalLoginMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: LocalLoginVars) =>
			api.global.post<AuthSessionResponse>(AUTH_ENDPOINTS.login, vars)
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
