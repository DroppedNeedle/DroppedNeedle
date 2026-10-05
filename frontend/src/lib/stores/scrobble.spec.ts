import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockGet = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: (...args: unknown[]) => mockGet(...args),
			post: vi.fn()
		}
	},
	ApiError: class extends Error {}
}));

import {
	scrobbleManager,
	makeNowPlayingSubmission,
	makeScrobbleSubmission
} from './scrobble.svelte';
import {
	makeTrackKey,
	shouldAccumulate,
	isLoopReset,
	shouldScrobble,
	SCROBBLE_TIME_THRESHOLD_MS,
	MIN_TRACK_DURATION_MS
} from './scrobbleHelpers';

describe('native scrobble payloads', () => {
	it('carry the playback source on now-playing and completed plays', () => {
		expect(makeNowPlayingSubmission('Artist', 'Song', 'Album', 180_000, 'navidrome')).toEqual({
			track_name: 'Song',
			artist_name: 'Artist',
			album_name: 'Album',
			duration_ms: 180_000,
			source: 'navidrome'
		});
		expect(makeScrobbleSubmission('Artist', 'Song', 'Album', 180_000, 1234, 'navidrome')).toEqual({
			track_name: 'Song',
			artist_name: 'Artist',
			album_name: 'Album',
			duration_ms: 180_000,
			timestamp: 1234,
			source: 'navidrome'
		});
	});
});

describe('scrobbleManager settings', () => {
	beforeEach(() => mockGet.mockReset());

	it.each([
		[true, false, true],
		[false, true, true],
		[false, false, false]
	])('lastfm=%s listenbrainz=%s enables=%s', async (lastfm, listenbrainz, enabled) => {
		mockGet.mockResolvedValueOnce({
			scrobble_to_lastfm: lastfm,
			scrobble_to_listenbrainz: listenbrainz
		});
		await scrobbleManager.refreshSettings();
		expect(scrobbleManager.enabled).toBe(enabled);
	});
});

describe('scrobble rules', () => {
	it.each([
		// listened ms, duration ms, expected
		[150_000, 300_000, true], // half of a 5 minute track
		[149_999, 300_000, false],
		[SCROBBLE_TIME_THRESHOLD_MS, 600_000, true], // 4 minute cap on long tracks
		[SCROBBLE_TIME_THRESHOLD_MS - 1, 600_000, false],
		[MIN_TRACK_DURATION_MS / 2, MIN_TRACK_DURATION_MS, true], // shortest accepted track
		[MIN_TRACK_DURATION_MS - 1, MIN_TRACK_DURATION_MS - 1, false] // too short to count
	])('listening %i ms of a %i ms track scrobbles: %s', (listened, duration, expected) => {
		expect(shouldScrobble(listened, duration, false)).toBe(expected);
	});

	it('never scrobbles the same play twice', () => {
		expect(shouldScrobble(200_000, 300_000, true)).toBe(false);
	});

	it.each([
		[1, true],
		[2.5, true],
		[0, false],
		[-5, false], // seek backward
		[3, false], // seek forward at the tolerance
		[10, false]
	])('a %s second progress step counts toward listening time: %s', (delta, expected) => {
		expect(shouldAccumulate(delta)).toBe(expected);
	});

	it('treats a jump from the end back to the start as a new play', () => {
		expect(isLoopReset(179, 0.5, 180_000)).toBe(true);
		expect(isLoopReset(90, 0.5, 180_000)).toBe(false);
		expect(isLoopReset(179, 5, 180_000)).toBe(false);
	});

	it('keys a track by artist and title, ignoring case', () => {
		expect(makeTrackKey('MUSE', 'Hysteria')).toBe(makeTrackKey('muse', 'hysteria'));
		expect(makeTrackKey('Muse', 'Hysteria')).not.toBe(makeTrackKey('Muse', 'Starlight'));
	});
});
