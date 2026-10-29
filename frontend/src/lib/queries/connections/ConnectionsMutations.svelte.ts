import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { authStore } from '$lib/stores/authStore.svelte';
import { pollPlexFlow, startPlexFlow } from '$lib/queries/plex/PlexFlowApi';
import type { PlexLinkPollResult } from '$lib/queries/plex/types';
import { REMOTE_ENDPOINTS } from '$lib/queries/remotes/endpoints';
import {
	applyEffects,
	connectionsChanged,
	createAppMutation,
	sourcePlaylistsChanged,
	spotifyPlaylistsChanged,
	type Effect
} from '../effects';
import { CONNECTIONS_ENDPOINTS } from './endpoints';
import type {
	LastFmAuthSessionResponse,
	LastFmAuthTokenResponse,
	ListenBrainzConnectVars,
	MediaServerConnectVars,
	PlexLinkPinResponse
} from './types';

type SourcePlaylistSource = components['schemas']['SourceName'];

const connections = (): Effect[] => [connectionsChanged(authStore.user?.id)];

/** Reads the caller's linked accounts again, after a link made outside the app. */
export const refreshConnections = () => applyEffects(...connections());

function isMediaSource(service: string): service is SourcePlaylistSource {
	return service === 'jellyfin' || service === 'navidrome' || service === 'plex';
}

function connectionAndPlaylists(service: string): Effect[] {
	const userId = authStore.user?.id;
	const effects = connections();
	if (isMediaSource(service)) effects.push(sourcePlaylistsChanged(userId, service));
	if (service === 'spotify') effects.push(spotifyPlaylistsChanged(userId));
	return effects;
}

// v3 keeps Last.fm app credentials per user: the user saves their own API
// key and shared secret before the OAuth steps below can run.
export const createSetLastFmCredentialsMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: components['schemas']['LastFmCredentialsSet']) =>
			api.v3.PUT(CONNECTIONS_ENDPOINTS.lastfm(), vars),
		effects: connections
	}));

// OAuth step 1: request a desktop token (no state change yet)
export const createLastFmRequestTokenMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: (): Promise<LastFmAuthTokenResponse> =>
			api.v3.POST(CONNECTIONS_ENDPOINTS.lastfmToken())
	}));

// OAuth step 2: exchange the approved token for a per-user session
export const createLastFmExchangeSessionMutation = () =>
	createAppMutation(() => ({
		mutationFn: (token: string): Promise<LastFmAuthSessionResponse> =>
			api.v3.POST(CONNECTIONS_ENDPOINTS.lastfmSession(), { token }),
		effects: connections
	}));

export const createConnectListenBrainzMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: ListenBrainzConnectVars) =>
			api.v3.PUT(CONNECTIONS_ENDPOINTS.listenbrainz(), vars),
		effects: connections
	}));

export const createDisconnectMutation = () =>
	createAppMutation(() => ({
		// One mutation fans out to several response shapes (plus a loud
		// rejection for a service with no unlink route); callers only await
		// settlement, so the result stays unknown.
		mutationFn: (service: string): Promise<unknown> => {
			if (isMediaSource(service)) {
				return api.v3.DELETE(REMOTE_ENDPOINTS.connection(service));
			}
			if (service === 'listenbrainz') {
				return api.v3.DELETE(CONNECTIONS_ENDPOINTS.listenbrainz());
			}
			if (service === 'lastfm') {
				return api.v3.DELETE(CONNECTIONS_ENDPOINTS.lastfm());
			}
			if (service === 'spotify') {
				return api.v3.DELETE(CONNECTIONS_ENDPOINTS.spotify());
			}
			// Anything else has no unlink route: fail loudly rather than
			// calling a route that does not exist.
			return Promise.reject(new Error(`Disconnect is not supported for ${service}`));
		},
		effects: (_data, service) => connectionAndPlaylists(service)
	}));

export const createConnectSpotifyMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async () => {
			const data = await api.v3.GET(CONNECTIONS_ENDPOINTS.spotifyAuthUrl());
			window.location.href = data.auth_url;
		}
	}));

// media-server account links (issue #138): credentials are validated live by the
// backend and never echoed back.
export const createConnectNavidromeMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: MediaServerConnectVars) =>
			api.v3.PUT(REMOTE_ENDPOINTS.connection('navidrome'), vars),
		effects: () => connectionAndPlaylists('navidrome')
	}));

export const createConnectJellyfinMutation = () =>
	createAppMutation(() => ({
		mutationFn: (vars: MediaServerConnectVars) =>
			api.v3.PUT(REMOTE_ENDPOINTS.connection('jellyfin'), vars),
		effects: () => connectionAndPlaylists('jellyfin')
	}));

// Plex OAuth step 1: mint a pin + popup URL (no state change yet). The v3
// start body names the popup URL `authorize_url`; the account card reads
// `auth_url`, so the mutation maps once here.
export const createPlexLinkPinMutation = () =>
	createAppMutation(() => ({
		effects: () => [],
		mutationFn: async (): Promise<PlexLinkPinResponse> => {
			const started = await startPlexFlow('link');
			return { pin_id: started.pin_id, auth_url: started.authorize_url };
		}
	}));

// Plex OAuth step 2: poll the pin; the backend persists the link on completion
export const createPlexLinkPollMutation = () =>
	createAppMutation(() => ({
		mutationFn: (pinId: number) => pollPlexFlow('link', pinId),
		effects: (data: PlexLinkPollResult) => (data.completed ? connectionAndPlaylists('plex') : [])
	}));
