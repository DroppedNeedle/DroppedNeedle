import type { QueueItem } from '$lib/player/types';
import { isPlexScrobbleEnabled } from '$lib/player/plexPlaybackApi';

interface ProgressReporterState {
	jellyfinItem: QueueItem | null;
	progress: number;
	isPaused: boolean;
}

type ReportProgressFn = (
	trackSourceId: string,
	playSessionId: string,
	progress: number,
	isPaused: boolean
) => Promise<boolean>;

export function createProgressReporter(
	reportProgress: ReportProgressFn,
	intervalMs: number,
	maxFailures: number
) {
	let interval: ReturnType<typeof setInterval> | null = null;
	let consecutiveFailures = 0;

	function start(getState: () => ProgressReporterState): void {
		stop();
		const item = getState().jellyfinItem;
		if (!item?.playSessionId) return;

		interval = setInterval(async () => {
			const { jellyfinItem, progress, isPaused } = getState();
			if (!jellyfinItem?.playSessionId) {
				stop();
				return;
			}
			try {
				const ok = await reportProgress(
					jellyfinItem.trackSourceId,
					jellyfinItem.playSessionId,
					progress,
					isPaused
				);
				if (ok) {
					consecutiveFailures = 0;
					return;
				}
				consecutiveFailures += 1;
				if (consecutiveFailures >= maxFailures) stop();
			} catch {
				// Ignore errors
			}
		}, intervalMs);
	}

	function stop(): void {
		if (interval) {
			clearInterval(interval);
			interval = null;
		}
		consecutiveFailures = 0;
	}

	return { start, stop };
}

function beacon(url: string, payload: Record<string, unknown>): void {
	navigator.sendBeacon(url, new Blob([JSON.stringify(payload)], { type: 'application/json' }));
}

function scrobbleBodyFrom(item: QueueItem): Record<string, unknown> {
	return {
		track_name: item.trackName,
		artist_name: item.artistName,
		album_name: item.albumName,
		timestamp: Math.floor(Date.now() / 1000),
		duration_ms: Math.round((item.duration ?? 0) * 1000),
		source: item.sourceType
	};
}

export function createBeforeUnloadHandler(
	getState: () => {
		jellyfinItem: QueueItem | null;
		currentItem: QueueItem | null;
		progress: number;
	},
	stopUrl: string,
	scrobbleUrl: string
): () => void {
	return () => {
		if (typeof navigator === 'undefined' || typeof navigator.sendBeacon !== 'function') return;
		const { jellyfinItem, currentItem, progress } = getState();

		if (jellyfinItem?.playSessionId) {
			beacon(stopUrl, {
				source: 'jellyfin',
				track_id: jellyfinItem.trackSourceId,
				position_ms: Math.round(progress * 1000)
			});
		}

		if (currentItem?.sourceType === 'navidrome' && progress > 30) {
			beacon(scrobbleUrl, scrobbleBodyFrom(currentItem));
		}

		if (
			currentItem?.sourceType === 'plex' &&
			currentItem.plexRatingKey &&
			progress > 30 &&
			isPlexScrobbleEnabled()
		) {
			beacon(scrobbleUrl, scrobbleBodyFrom(currentItem));
		}
	};
}
