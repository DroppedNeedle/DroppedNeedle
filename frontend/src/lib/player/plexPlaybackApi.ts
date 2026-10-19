import { ApiError } from '$lib/api/client';

import { startPlaybackSession, stopPlaybackSession } from './playbackGateway';

// Thin Plex leg over the single gateway session flow: rating keys travel as
// the catalog track id. A natural end and an early stop are both session
// ends in v3; scrobble accounting moved server-side onto the stop threshold.
// A track's end always sends its stop report.
export async function reportPlexScrobble(ratingKey: string): Promise<void> {
	try {
		await stopPlaybackSession({ source: 'plex', track_id: ratingKey });
	} catch (e) {
		const detail = e instanceof ApiError ? String(e.status) : 'network error';
		console.warn(`[Plex] scrobble failed: ${detail}`);
	}
}

export async function reportPlexNowPlaying(ratingKey: string): Promise<void> {
	try {
		await startPlaybackSession({ source: 'plex', track_id: ratingKey });
	} catch (e) {
		const detail = e instanceof ApiError ? String(e.status) : 'network error';
		console.warn(`[Plex] now-playing failed: ${detail}`);
	}
}

export async function reportPlexStopped(ratingKey: string): Promise<void> {
	try {
		await stopPlaybackSession({ source: 'plex', track_id: ratingKey });
	} catch (e) {
		const detail = e instanceof ApiError ? String(e.status) : 'network error';
		console.warn(`[Plex] stopped report failed: ${detail}`);
	}
}
