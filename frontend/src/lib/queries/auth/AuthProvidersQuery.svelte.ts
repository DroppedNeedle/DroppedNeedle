import { api } from '$lib/api/client';
import { createQuery } from '@tanstack/svelte-query';
import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';
import { API } from '$lib/constants';
import type { AuthProviders } from './types';

/** Which login methods the server has enabled. Unauthenticated; safe on /login.
 * Untyped by necessity: the backend has no spec entry for this route yet, so
 * its builder waits in lib/constants.ts. */
export const getAuthProvidersQuery = () =>
	createQuery(() => ({
		queryKey: AuthQueryKeyFactory.providers(),
		queryFn: ({ signal }) => api.global.get<AuthProviders>(API.auth.providers(), { signal })
	}));
