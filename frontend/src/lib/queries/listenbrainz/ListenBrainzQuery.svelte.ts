import { api } from '$lib/api/client';
import { createQuery, queryOptions } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { ListenBrainzQueryKeyFactory } from './ListenBrainzQueryKeyFactory';
import { LISTENBRAINZ_ENDPOINTS } from './endpoints';

const LISTENBRAINZ_TIMEOUT_MS = 10_000;

export const getListenBrainzConnectionQueryOptions = () =>
	queryOptions({
		queryKey: ListenBrainzQueryKeyFactory.connection(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LISTENBRAINZ_ENDPOINTS.connection, {
				signal,
				timeoutMs: LISTENBRAINZ_TIMEOUT_MS
			})
	});

export const getListenBrainzConnectionQuery = () =>
	createQuery(() => ({
		...getListenBrainzConnectionQueryOptions(),
		enabled: authStore.isAdmin
	}));

export const getScrobbleTargetsQueryOptions = () =>
	queryOptions({
		queryKey: ListenBrainzQueryKeyFactory.scrobble(),
		queryFn: ({ signal }) =>
			api.global.v3.GET(LISTENBRAINZ_ENDPOINTS.scrobble, {
				signal,
				timeoutMs: LISTENBRAINZ_TIMEOUT_MS
			})
	});

export const getScrobbleTargetsQuery = () =>
	createQuery(() => ({
		...getScrobbleTargetsQueryOptions(),
		enabled: authStore.isAdmin
	}));
