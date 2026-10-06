import { v3 } from '$lib/api/v3/endpoint';

// v3 connection URLs, built through the typed registry: every template is a
// literal the contract-coverage gate verifies against the generated spec.
// Media-server links (navidrome/jellyfin/plex) ride the remotes slice's
// connection entry - hooks import REMOTE_ENDPOINTS for those rather than
// duplicating the template here. The current user is resolved server-side
// from the session cookie, so no endpoint takes a user id.
export const CONNECTIONS_ENDPOINTS = {
	list: () => v3('/api/v3/me/connections'),
	listenbrainz: () => v3('/api/v3/me/connections/listenbrainz'),
	lastfm: () => v3('/api/v3/me/connections/lastfm'),
	lastfmToken: () => v3('/api/v3/me/connections/lastfm/token'),
	lastfmSession: () => v3('/api/v3/me/connections/lastfm/session'),
	// Plex link pins ride the single v3 Plex flow ($lib/queries/plex).
	spotify: () => v3('/api/v3/me/connections/spotify'),
	spotifyAuthUrl: () => v3('/api/v3/acquire/spotify/auth/url')
} as const;
