export const SCROBBLE_PERCENT_THRESHOLD = 0.5;
export const SCROBBLE_TIME_THRESHOLD_MS = 240_000;
export const NOW_PLAYING_DEBOUNCE_MS = 3000;
const SEEK_TOLERANCE_S = 3;
export const MIN_TRACK_DURATION_MS = 30_000;
export const LOOP_RESET_TOLERANCE_S = 3;

export function makeTrackKey(artistName: string, trackName: string): string {
	return `${artistName.toLowerCase()}::${trackName.toLowerCase()}`;
}

export function shouldAccumulate(deltaS: number): boolean {
	return deltaS > 0 && deltaS < SEEK_TOLERANCE_S;
}

export function isLoopReset(
	prevProgressS: number,
	currentProgressS: number,
	durationMs: number
): boolean {
	if (durationMs <= 0 || prevProgressS <= 0) return false;
	const durationS = durationMs / 1000;
	return (
		prevProgressS >= durationS - LOOP_RESET_TOLERANCE_S && currentProgressS < LOOP_RESET_TOLERANCE_S
	);
}

export function shouldSendNowPlaying(accumulatedMs: number, alreadySent: boolean): boolean {
	return !alreadySent && accumulatedMs >= NOW_PLAYING_DEBOUNCE_MS;
}

export function getScrobbleThreshold(durationMs: number): number {
	const halfDuration = durationMs * SCROBBLE_PERCENT_THRESHOLD;
	return Math.min(halfDuration, SCROBBLE_TIME_THRESHOLD_MS);
}

export function shouldScrobble(
	accumulatedMs: number,
	durationMs: number,
	alreadyScrobbled: boolean
): boolean {
	if (alreadyScrobbled) return false;
	if (durationMs < MIN_TRACK_DURATION_MS) return false;
	return accumulatedMs >= getScrobbleThreshold(durationMs);
}

export function formatServiceTooltip(
	scrobbleStatus: string,
	serviceDetail: Record<string, { queued: boolean }> | null
): string {
	if (scrobbleStatus === 'tracking') return 'Tracking';
	if (!serviceDetail || Object.keys(serviceDetail).length === 0) {
		return scrobbleStatus === 'scrobbled' ? 'Scrobbled' : 'Scrobble failed';
	}
	const queued = Object.entries(serviceDetail)
		.filter(([, v]) => v.queued)
		.map(([k]) => (k === 'lastfm' ? 'Last.fm' : k === 'listenbrainz' ? 'ListenBrainz' : k));
	const failed = Object.entries(serviceDetail)
		.filter(([, v]) => !v.queued)
		.map(([k]) => (k === 'lastfm' ? 'Last.fm' : k === 'listenbrainz' ? 'ListenBrainz' : k));
	const parts: string[] = [];
	if (queued.length) parts.push(`Scrobbling to ${queued.join(', ')}`);
	if (failed.length) parts.push(`Failed: ${failed.join(', ')}`);
	return parts.join(' · ') || 'Scrobbled';
}
