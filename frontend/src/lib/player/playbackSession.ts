import { beaconAvailable, sendJsonBeacon } from '$lib/api/channels';
import {
	GATEWAY_ENDPOINTS,
	reportPlaybackProgress,
	startPlaybackSession,
	stopPlaybackSession,
	type PlaybackStopRequest,
	type RemoteSource
} from './playbackGateway';
import type { QueueItem } from './types';

export interface SessionKey {
	source: RemoteSource;
	trackId: string;
}

/**
 * The session a track reports under, or null for a track that reports none.
 * Only a media-server track opens one, and the server passes its reports on
 * to that media server; the play itself is counted by the scrobble submit.
 * Plex reports under its rating key, which a part-key-only item lacks.
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
	/** The track's length in seconds, 0 while it is not known. */
	duration: () => number;
}

const toMs = (seconds: number) => Math.round(seconds * 1000);

/**
 * One playback session at a time, reported to the gateway: start when the
 * track first plays, progress every interval while it plays and once on
 * pause, stop when it ends or is left. A page unload sends the stop as a
 * beacon. A track that was loaded but never played (a restored queue)
 * reports nothing, so a reload never counts a play twice. Reports are
 * best-effort: a failed one never touches playback.
 */
export function createPlaybackSession(options: SessionOptions) {
	let active: SessionKey | null = null;
	let interval: ReturnType<typeof setInterval> | null = null;
	let failures = 0;
	let started = false;
	let lengthMs: number | undefined;

	function stopInterval(): void {
		if (interval) clearInterval(interval);
		interval = null;
		failures = 0;
	}

	// The player forgets a track's length once the next one loads, so the
	// session keeps the last length it read for its stop.
	function durationMs(): number | undefined {
		const seconds = options.duration();
		if (Number.isFinite(seconds) && seconds > 0) lengthMs = toMs(seconds);
		return lengthMs;
	}

	async function sendProgress(key: SessionKey, seconds: number, isPaused: boolean) {
		await reportPlaybackProgress({
			source: key.source,
			track_id: key.trackId,
			position_ms: toMs(seconds),
			duration_ms: durationMs(),
			is_paused: isPaused
		});
	}

	function handleBeforeUnload(): void {
		if (!active || !beaconAvailable()) return;
		const stop: PlaybackStopRequest = {
			source: active.source,
			track_id: active.trackId,
			position_ms: toMs(options.position().seconds),
			duration_ms: durationMs()
		};
		sendJsonBeacon(GATEWAY_ENDPOINTS.playbackStop(), stop);
	}

	function listen(on: boolean): void {
		if (typeof window === 'undefined') return;
		if (on) window.addEventListener('beforeunload', handleBeforeUnload);
		else window.removeEventListener('beforeunload', handleBeforeUnload);
	}

	/** Forget the session without reporting it (the player was reset). */
	function clear(): void {
		stopInterval();
		if (started) listen(false);
		active = null;
		started = false;
		lengthMs = undefined;
	}

	return {
		/**
		 * Takes the track a session would report under; a track with no key
		 * opens none. Nothing is sent until it plays.
		 */
		begin(key: SessionKey | null): void {
			clear();
			active = key;
		},

		/** Playback started or resumed: report progress on the interval. */
		playing(): void {
			stopInterval();
			const key = active;
			if (!key) return;
			if (!started) {
				started = true;
				listen(true);
				void startPlaybackSession({
					source: key.source,
					track_id: key.trackId,
					position_ms: toMs(options.position().seconds),
					duration_ms: durationMs()
				}).catch(() => {
					// The session stays open: its later reports are sent regardless.
				});
			}
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
			if (!active || !started) return;
			void sendProgress(active, seconds, true).catch(() => {});
		},

		/** Closes the open session with its final position. */
		async end(seconds: number): Promise<void> {
			const key = started ? active : null;
			const length = durationMs();
			clear();
			if (!key) return;
			try {
				await stopPlaybackSession({
					source: key.source,
					track_id: key.trackId,
					position_ms: toMs(seconds),
					duration_ms: length
				});
			} catch {
				// Best-effort, like every session report.
			}
		},

		clear
	};
}
