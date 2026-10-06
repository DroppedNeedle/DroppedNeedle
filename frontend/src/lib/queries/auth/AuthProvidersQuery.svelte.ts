import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { AUTH_ENDPOINTS } from './endpoints';
import type { AuthProviders } from './types';

/** Which login methods the server has enabled. Unauthenticated; safe on /login. */
export const getAuthProvidersQuery = () =>
	createQuery(() => ({
		queryKey: AuthQueryKeyFactory.providers(),
		queryFn: ({ signal }): Promise<AuthProviders> =>
			api.global.v3.GET(AUTH_ENDPOINTS.providers, { signal })
	}));
