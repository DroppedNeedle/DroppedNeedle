import { v3 } from '$lib/api/v3/endpoint';

// v3 Spotify import URLs, built through the typed registry: every template
// is a literal the contract-coverage gate verifies against the generated
// spec. The caller is resolved server-side from the session cookie.
export const SPOTIFY_ENDPOINTS = {
	settings: () => v3('/api/v3/acquire/spotify/settings'),
	redirectUri: () => v3('/api/v3/acquire/spotify/redirect-uri'),
	authCallback: () => v3('/api/v3/acquire/spotify/auth/callback'),
	playlists: () => v3('/api/v3/acquire/spotify/playlists'),
	importPlaylist: (playlistId: string) =>
		v3('/api/v3/acquire/spotify/playlists/{id}/import', { path: { id: playlistId } })
} as const;
