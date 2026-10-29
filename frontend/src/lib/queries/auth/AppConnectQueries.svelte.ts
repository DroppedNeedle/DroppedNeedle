import { createQuery, skipToken } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import { v3, type V3QueryOf } from '$lib/api/v3/endpoint';
import type { components } from '$lib/api/v3/openapi';
import { createAppMutation } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';

import { AuthQueryKeyFactory } from './AuthQueryKeyFactory';

/** What an app asks for at /connect, as its query. */
export type AppConnectQuery = V3QueryOf<'/api/v3/auth/app-connect/request'>;
export type AppConnectRequestView = components['schemas']['AppConnectRequestView'];
export type ConnectRequest = components['schemas']['ConnectRequest'];

/**
 * What a native app asks to connect as, read once for the page that shows it: never retried,
 * refetched or kept, since a refusal is the answer and the request is good only for now.
 * `null` reads nothing.
 */
export const getAppConnectRequestQuery = (query: () => AppConnectQuery | null) =>
	createQuery(() => {
		const asked = query();
		return {
			queryKey: AuthQueryKeyFactory.appConnectRequest(authStore.user?.id, asked),
			retry: false,
			gcTime: 0,
			staleTime: Infinity,
			refetchOnWindowFocus: false,
			queryFn: asked
				? ({ signal }: { signal: AbortSignal }) =>
						api.v3.GET(v3('/api/v3/auth/app-connect/request', { query: asked }), { signal })
				: skipToken
		};
	});

/** Allow the app: the answer is the address that hands the code back to it. */
export function createAuthorizeAppConnectMutation() {
	return createAppMutation(() => ({
		mutationFn: (request: ConnectRequest) =>
			api.v3.POST(v3('/api/v3/auth/app-connect/authorize'), request),
		effects: () => []
	}));
}
