import { API } from '$lib/constants';

// current user is resolved server-side from the session cookie, so no endpoint takes a user id
export const CONNECTIONS_ENDPOINTS = {
	list: API.me.connections(),
	connection: (service: string) => API.me.connection(service),
	lastfmAuthToken: API.me.lastfmAuthToken(),
	lastfmAuthSession: API.me.lastfmAuthSession(),
	listenbrainz: API.me.listenbrainz(),
	spotifyAuthUrl: API.me.spotifyAuthUrl()
} as const;
