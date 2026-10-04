import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { ScrobblePreferencesQueryKeyFactory } from './ScrobblePreferencesQueryKeyFactory';
import { SCROBBLE_PREFERENCES_ENDPOINTS } from './endpoints';

const SCROBBLE_PREFERENCES_TIMEOUT_MS = 10_000;

export const getScrobblePreferencesQueryOptions = (userId: string | undefined) =>
	queryOptions({
		queryKey: ScrobblePreferencesQueryKeyFactory.get(userId),
		queryFn: ({ signal }) =>
			api.global.v3.GET(SCROBBLE_PREFERENCES_ENDPOINTS.get(), {
				signal,
				timeoutMs: SCROBBLE_PREFERENCES_TIMEOUT_MS
			}),
		staleTime: Infinity,
		gcTime: Infinity
	});

export const getScrobblePreferencesQuery = () =>
	createQuery(() => ({
		...getScrobblePreferencesQueryOptions(authStore.user?.id),
		enabled: Boolean(authStore.user?.id)
	}));
