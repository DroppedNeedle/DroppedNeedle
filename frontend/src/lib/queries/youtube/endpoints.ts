import { v3 } from '$lib/api/v3/endpoint';

// Saved YouTube links (album bar, track buttons, the YouTube library page).
// Links are shared by everyone on the server.
export const YOUTUBE_ENDPOINTS = {
	generate: () => v3('/api/v3/youtube/generate'),
	link: (albumId: string) => v3('/api/v3/youtube/link/{album_id}', { path: { album_id: albumId } }),
	links: () => v3('/api/v3/youtube/links'),
	manual: () => v3('/api/v3/youtube/manual'),
	generateTrack: () => v3('/api/v3/youtube/generate-track'),
	generateTracks: () => v3('/api/v3/youtube/generate-tracks'),
	trackLinks: (albumId: string) =>
		v3('/api/v3/youtube/track-links/{album_id}', { path: { album_id: albumId } })
};
