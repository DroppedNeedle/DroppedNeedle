import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import type { AuthProviders } from './types';

/** Which login methods the server has enabled. Unauthenticated; safe on /login. */
export const getAuthProvidersQuery = () =>
	createQuery(() => ({
		queryKey: AuthQueryKeyFactory.providers(),
		queryFn: ({ signal }): Promise<AuthProviders> =>
			api.v3.GET(v3('/api/v3/auth/providers'), { signal })
	}));
