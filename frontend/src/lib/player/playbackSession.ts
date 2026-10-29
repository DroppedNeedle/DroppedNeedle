import { beaconAvailable, sendJsonBeacon } from '$lib/api/channels';
import {
	GATEWAY_ENDPOINTS,
	reportPlaybackProgress,
	startPlaybackSession,
	stopPlaybackSession,
	type GatewaySource
} from './playbackGateway';
import type { QueueItem } from './types';

export interface SessionKey {
	source: GatewaySource;
	trackId: string;
}

/**
 * The session a track reports under, or null for a track that reports none.
 * Local plays are counted by the scrobble submit and YouTube plays by nobody,
 * so only the media-server sources open a session. Plex reports under its
 * rating key, which a part-key-only item lacks.
 */
export function sessionKeyFor(item: QueueItem): SessionKey | null {
	switch (item.sourceType) {
		case 'jellyfin':
		case 'navidrome':
			return { source: item.sourceType, trackId: item.trackSourceId };
		case 'plex':
			return item.plexRatingKey ? { source: 'plex', trackId: item.plexRatingKey } : null;
		default:
			return null;
	}
}

interface SessionOptions {
	intervalMs: number;
	maxFailures: number;
	position: () => { seconds: number; isPaused: boolean };
}

const toMs = (seconds: number) => Math.round(seconds * 1000);

/**
 * One playback session at a time, reported to the gateway: start when a track
 * loads, progress every interval while it plays and once on pause, stop when
 * it ends or is left. A page unload sends the stop as a beacon. Reports are
 * best-effort: a failed one never touches playback.
 */
export function createPlaybackSession(options: SessionOptions) {
	let active: SessionKey | null = null;
	let interval: ReturnType<typeof setInterval> | null = null;
	let failures = 0;

	function stopInterval(): void {
		if (interval) clearInterval(interval);
		interval = null;
		failures = 0;
	}

	async function sendProgress(key: SessionKey, seconds: number, isPaused: boolean) {
		await reportPlaybackProgress({
			source: key.source,
			track_id: key.trackId,
			position_ms: toMs(seconds),
			is_paused: isPaused
		});
	}

	function handleBeforeUnload(): void {
		if (!active || !beaconAvailable()) return;
		sendJsonBeacon(GATEWAY_ENDPOINTS.playbackStop(), {
			source: active.source,
			track_id: active.trackId,
			position_ms: toMs(options.position().seconds)
		});
	}

	function listen(on: boolean): void {
		if (typeof window === 'undefined') return;
		if (on) window.addEventListener('beforeunload', handleBeforeUnload);
		else window.removeEventListener('beforeunload', handleBeforeUnload);
	}

	/** Forget the session without reporting it (the player was reset). */
	function clear(): void {
		stopInterval();
		if (active) listen(false);
		active = null;
	}

	return {
		/** Opens a session for a track; a track with no key opens none. */
		async begin(key: SessionKey | null): Promise<void> {
			clear();
			if (!key) return;
			active = key;
			listen(true);
			try {
				await startPlaybackSession({ source: key.source, track_id: key.trackId });
			} catch {
				// The session stays open: its later reports are sent regardless.
			}
		},

		/** Playback started or resumed: report progress on the interval. */
		playing(): void {
			stopInterval();
			const key = active;
			if (!key) return;
			interval = setInterval(async () => {
				if (active !== key) return stopInterval();
				const { seconds, isPaused } = options.position();
				try {
					await sendProgress(key, seconds, isPaused);
					failures = 0;
				} catch {
					failures += 1;
					if (failures >= options.maxFailures) stopInterval();
				}
			}, options.intervalMs);
		},

		paused(seconds: number): void {
			if (!active) return;
			void sendProgress(active, seconds, true).catch(() => {});
		},

		/** Closes the open session with its final position. */
		async end(seconds: number): Promise<void> {
			const key = active;
			clear();
			if (!key) return;
			try {
				await stopPlaybackSession({
					source: key.source,
					track_id: key.trackId,
					position_ms: toMs(seconds)
				});
			} catch {
				// Best-effort, like every session report.
			}
		},

		clear
	};
}
