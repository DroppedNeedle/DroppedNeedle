import { ApiError } from '$lib/api/client';

import { reportPlaybackProgress, startPlaybackSession, stopPlaybackSession } from './playbackGateway';

// Thin Jellyfin leg over the single gateway session flow. Signatures stay
// stable so the player store keeps compiling; the v1 play-session id the
// player threads through is now the gateway's opaque session key.

export async function startSession(itemId: string, _playSessionId?: string): Promise<string> {
	// v3 sessions are server-keyed per (user, device, track): there is no
	// resume, so a carried session id is ignored and each start is fresh.
	try {
		const data = await startPlaybackSession({ source: 'jellyfin', track_id: itemId });
		return data.session;
	} catch (e) {
		if (e instanceof ApiError) {
			throw new Error(`Failed to start Jellyfin playback session: ${e.status} ${e.message}`, {
				cause: e
			});
		}
		throw e;
	}
}

export async function reportProgress(
	itemId: string,
	_playSessionId: string,
	positionSeconds: number,
	isPaused: boolean
): Promise<boolean> {
	try {
		await reportPlaybackProgress({
			source: 'jellyfin',
			track_id: itemId,
			position_ms: Math.round(positionSeconds * 1000),
			is_paused: isPaused
		});
		return true;
	} catch {
		return false;
	}
}

export async function reportStop(
	itemId: string,
	_playSessionId: string,
	positionSeconds: number
): Promise<boolean> {
	try {
		await stopPlaybackSession({
			source: 'jellyfin',
			track_id: itemId,
			position_ms: Math.round(positionSeconds * 1000)
		});
		return true;
	} catch {
		return false;
	}
}
