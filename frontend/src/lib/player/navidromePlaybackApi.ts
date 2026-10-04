import { startPlaybackSession, stopPlaybackSession } from './playbackGateway';

// Thin Navidrome leg over the single gateway session flow. A natural end and
// an early stop are both session ends in v3; scrobble accounting moved
// server-side onto the stop threshold.

export async function reportNavidromeScrobble(itemId: string): Promise<void> {
	try {
		await stopPlaybackSession({ source: 'navidrome', track_id: itemId });
	} catch {
		// best-effort scrobble
	}
}

export async function reportNavidromeNowPlaying(itemId: string): Promise<void> {
	try {
		await startPlaybackSession({ source: 'navidrome', track_id: itemId });
	} catch {
		// best-effort now-playing report
	}
}

export async function reportNavidromeStopped(itemId: string): Promise<void> {
	try {
		await stopPlaybackSession({ source: 'navidrome', track_id: itemId });
	} catch {
		// best-effort stopped report
	}
}
