import { api, ApiError } from '$lib/api/client';

import { startPlaybackSession, stopPlaybackSession } from './playbackGateway';
import { PLEX_ENDPOINTS } from '$lib/queries/plex/endpoints';

let scrobbleEnabled: boolean | null = null;

async function loadScrobblePreference(): Promise<boolean> {
	if (scrobbleEnabled !== null) return scrobbleEnabled;
	try {
		const settings = await api.global.v3.GET(PLEX_ENDPOINTS.settings());
		scrobbleEnabled = settings.scrobble_to_plex ?? false;
	} catch {
		return false;
	}
	return scrobbleEnabled;
}

export function isPlexScrobbleEnabled(): boolean {
	return scrobbleEnabled ?? false;
}

export function resetPlexScrobblePreference(): void {
	scrobbleEnabled = null;
}

// Thin Plex leg over the single gateway session flow: rating keys travel as
// the catalog track id. A natural end and an early stop are both session
// ends in v3; scrobble accounting moved server-side onto the stop threshold.
export async function reportPlexScrobble(ratingKey: string): Promise<void> {
	if (!(await loadScrobblePreference())) return;
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
