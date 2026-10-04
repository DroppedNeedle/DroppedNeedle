import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { SessionsQueryKeyFactory } from './SessionsQueryKeyFactory';
import { SESSIONS_ENDPOINTS } from './endpoints';

const SESSIONS_TIMEOUT_MS = 10_000;

export const getSessionsQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: SessionsQueryKeyFactory.list(userId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(SESSIONS_ENDPOINTS.list(), {
				signal,
				timeoutMs: SESSIONS_TIMEOUT_MS
			})
	});

export const getSessionsQuery = () =>
	createQuery(() => ({
		...getSessionsQueryOptions(authStore.user?.id),
		enabled: Boolean(authStore.user?.id)
	}));
