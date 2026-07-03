import { api } from '$lib/api/client';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { ConnectionsQueryKeyFactory } from './ConnectionsQueryKeyFactory';
import { CONNECTIONS_ENDPOINTS } from './endpoints';
import type {
	ConnectionActionResponse,
	LastFmAuthSessionResponse,
	LastFmAuthTokenResponse,
	ListenBrainzConnectVars
} from './types';

function invalidateConnections(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: ConnectionsQueryKeyFactory.list(authStore.user?.id)
	});
}

// OAuth step 1: request a desktop token (no state change yet)
export const createLastFmRequestTokenMutation = () =>
	createMutation(() => ({
		mutationFn: () =>
			api.global.post<LastFmAuthTokenResponse>(CONNECTIONS_ENDPOINTS.lastfmAuthToken)
	}));

// OAuth step 2: exchange the approved token for a per-user session
export const createLastFmExchangeSessionMutation = () =>
	createMutation(() => ({
		mutationFn: (token: string) =>
			api.global.post<LastFmAuthSessionResponse>(CONNECTIONS_ENDPOINTS.lastfmAuthSession, {
				token
			}),
		onSuccess: invalidateConnections
	}));

export const createConnectListenBrainzMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ListenBrainzConnectVars) =>
			api.global.put<ConnectionStatusResponse>(CONNECTIONS_ENDPOINTS.listenbrainz, vars),
		onSuccess: invalidateConnections
	}));

export const createDisconnectMutation = () =>
	createMutation(() => ({
		mutationFn: (service: string) =>
			api.global.delete<ConnectionActionResponse>(CONNECTIONS_ENDPOINTS.connection(service)),
		onSuccess: invalidateConnections
	}));

export const createConnectSpotifyMutation = () =>
	createMutation(() => ({
		mutationFn: async () => {
			const data = await api.global.get<{ auth_url: string }>(CONNECTIONS_ENDPOINTS.spotifyAuthUrl);
			window.location.href = data.auth_url;
		}
	}));

type ConnectionStatusResponse = { service: string; enabled: boolean; username: string };
