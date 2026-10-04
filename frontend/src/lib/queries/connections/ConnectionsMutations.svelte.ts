import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { createMutation } from '@tanstack/svelte-query';
import { authStore } from '$lib/stores/authStore.svelte';
import { pollPlexFlow, startPlexFlow } from '$lib/queries/plex/PlexFlowApi';
import type { PlexLinkPollResult } from '$lib/queries/plex/types';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import { invalidateQueriesWithPersister } from '../QueryClient';
import { ConnectionsQueryKeyFactory } from './ConnectionsQueryKeyFactory';
import { CONNECTIONS_ENDPOINTS } from './endpoints';
import type {
	LastFmAuthSessionResponse,
	LastFmAuthTokenResponse,
	ListenBrainzConnectVars,
	MediaServerConnectVars,
	MediaServerConnectionStatus,
	PlexLinkPinResponse
} from './types';
import { SourcePlaylistQueryKeyFactory } from '$lib/queries/source-playlists/SourcePlaylistQueryKeyFactory';

type SourcePlaylistSource = components['schemas']['SourceName'];

function invalidateConnections(): Promise<void> {
	return invalidateQueriesWithPersister({
		queryKey: ConnectionsQueryKeyFactory.list(authStore.user?.id)
	});
}

function isMediaSource(service: string): service is SourcePlaylistSource {
	return service === 'jellyfin' || service === 'navidrome' || service === 'plex';
}

async function invalidateConnectionAndPlaylists(service?: string): Promise<void> {
	await invalidateConnections();
	if (service && isMediaSource(service)) {
		await invalidateQueriesWithPersister({
			queryKey: SourcePlaylistQueryKeyFactory.source(authStore.user?.id, service)
		});
	}
}

// OAuth step 1: request a desktop token (no state change yet)
export const createLastFmRequestTokenMutation = () =>
	createMutation(() => ({
		mutationFn: (): Promise<LastFmAuthTokenResponse> =>
			api.global.v3.POST(CONNECTIONS_ENDPOINTS.lastfmToken())
	}));

// OAuth step 2: exchange the approved token for a per-user session
export const createLastFmExchangeSessionMutation = () =>
	createMutation(() => ({
		mutationFn: (token: string): Promise<LastFmAuthSessionResponse> =>
			api.global.v3.POST(CONNECTIONS_ENDPOINTS.lastfmSession(), { token }),
		onSuccess: invalidateConnections
	}));

export const createConnectListenBrainzMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: ListenBrainzConnectVars) =>
			api.global.v3.PUT(CONNECTIONS_ENDPOINTS.listenbrainz(), vars),
		onSuccess: invalidateConnections
	}));

export const createDisconnectMutation = () =>
	createMutation(() => ({
		// One mutation fans out to three response shapes (plus a loud
		// rejection for services v3 cannot unlink); callers only await
		// settlement, so the result stays unknown.
		mutationFn: (service: string): Promise<unknown> => {
			if (isMediaSource(service)) {
				return api.global.v3.DELETE(REMOTE_ENDPOINTS.connection(service));
			}
			if (service === 'listenbrainz') {
				return api.global.v3.DELETE(CONNECTIONS_ENDPOINTS.listenbrainz());
			}
			if (service === 'lastfm') {
				return api.global.v3.DELETE(CONNECTIONS_ENDPOINTS.lastfm());
			}
			// v3 ships no disconnect for spotify (or anything else): fail
			// loudly rather than calling a route that does not exist.
			return Promise.reject(new Error(`Disconnect is not supported for ${service}`));
		},
		onSuccess: (_data, service) => invalidateConnectionAndPlaylists(service)
	}));

export const createConnectSpotifyMutation = () =>
	createMutation(() => ({
		mutationFn: async () => {
			const data = await api.global.v3.GET(CONNECTIONS_ENDPOINTS.spotifyAuthUrl());
			window.location.href = data.auth_url;
		}
	}));

// media-server account links (issue #138): credentials are validated live by the
// backend and never echoed back. Untyped by necessity: the PUT connection
// route takes a username/password JSON body, but the spec annotates no
// requestBody, so the typed client would reject the body. The URL still
// comes from the remotes registry entry. Revisit once the backend annotates it.
export const createConnectNavidromeMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: MediaServerConnectVars) =>
			api.global.put<MediaServerConnectionStatus>(REMOTE_ENDPOINTS.connection('navidrome'), vars),
		onSuccess: () => invalidateConnectionAndPlaylists('navidrome')
	}));

export const createConnectJellyfinMutation = () =>
	createMutation(() => ({
		mutationFn: (vars: MediaServerConnectVars) =>
			api.global.put<MediaServerConnectionStatus>(REMOTE_ENDPOINTS.connection('jellyfin'), vars),
		onSuccess: () => invalidateConnectionAndPlaylists('jellyfin')
	}));

// Plex OAuth step 1: mint a pin + popup URL (no state change yet). The v3
// start body names the popup URL `authorize_url`; the account card reads
// `auth_url`, so the mutation maps once here.
export const createPlexLinkPinMutation = () =>
	createMutation(() => ({
		mutationFn: async (): Promise<PlexLinkPinResponse> => {
			const started = await startPlexFlow('link');
			return { pin_id: started.pin_id, auth_url: started.authorize_url };
		}
	}));

// Plex OAuth step 2: poll the pin; the backend persists the link on completion
export const createPlexLinkPollMutation = () =>
	createMutation(() => ({
		mutationFn: (pinId: number) => pollPlexFlow('link', pinId),
		onSuccess: (data: PlexLinkPollResult) =>
			data.completed ? invalidateConnectionAndPlaylists('plex') : Promise.resolve()
	}));
