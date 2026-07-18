import { describe, it, expect, vi, beforeEach } from 'vitest';

const mockGet = vi.fn();
const mockPost = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: (...args: unknown[]) => mockGet(...args),
			post: (...args: unknown[]) => mockPost(...args)
		}
	},
	ApiError: class extends Error {
		status: number;
		code: string;
		details: unknown;
		constructor(s: number, c: string, m: string, d?: unknown) {
			super(m);
			this.status = s;
			this.code = c;
			this.details = d;
		}
	}
}));

import {
	scrobbleManager,
	formatServiceTooltip,
	makeNowPlayingSubmission,
	makeScrobbleSubmission
} from './scrobble.svelte';
import {
	makeTrackKey,
	shouldAccumulate,
	isLoopReset,
	shouldSendNowPlaying,
	shouldScrobble,
	getScrobbleThreshold,
	SCROBBLE_PERCENT_THRESHOLD,
	SCROBBLE_TIME_THRESHOLD_MS,
	NOW_PLAYING_DEBOUNCE_MS,
	MIN_TRACK_DURATION_MS,
	LOOP_RESET_TOLERANCE_S
} from './scrobbleHelpers';

describe('formatServiceTooltip', () => {
	it('returns "Tracking" when status is tracking', () => {
		expect(formatServiceTooltip('tracking', null)).toBe('Tracking');
	});

	it('returns "Scrobbled" when scrobbled with no detail', () => {
		expect(formatServiceTooltip('scrobbled', null)).toBe('Scrobbled');
	});

	it('returns "Scrobbled" when scrobbled with empty detail', () => {
		expect(formatServiceTooltip('scrobbled', {})).toBe('Scrobbled');
	});

	it('lists successful services', () => {
		const detail = {
			lastfm: { success: true },
			listenbrainz: { success: true }
		};
		expect(formatServiceTooltip('scrobbled', detail)).toBe('Scrobbled to Last.fm, ListenBrainz');
	});

	it('lists failed services', () => {
		const detail = {
			lastfm: { success: false }
		};
		expect(formatServiceTooltip('error', detail)).toBe('Failed: Last.fm');
	});

	it('shows mixed results with separator', () => {
		const detail = {
			lastfm: { success: true },
			listenbrainz: { success: false }
		};
		const result = formatServiceTooltip('scrobbled', detail);
		expect(result).toContain('Scrobbled to Last.fm');
		expect(result).toContain('Failed: ListenBrainz');
		expect(result).toContain(' · ');
	});

	it('returns "Scrobble failed" for error with no detail', () => {
		expect(formatServiceTooltip('error', null)).toBe('Scrobble failed');
	});

	it('passes through unknown service names as-is', () => {
		const detail = { spotify: { success: true } };
		expect(formatServiceTooltip('scrobbled', detail)).toBe('Scrobbled to spotify');
	});
});

describe('native scrobble payloads', () => {
	it('include the playback source in now-playing reports', () => {
		expect(makeNowPlayingSubmission('Artist', 'Song', 'Album', 180_000, 'navidrome')).toEqual({
			track_name: 'Song',
			artist_name: 'Artist',
			album_name: 'Album',
			duration_ms: 180_000,
			source: 'navidrome'
		});
	});

	it('include the playback source in completed scrobbles', () => {
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

describe('scrobbleManager', () => {
	it('starts idle and disabled', () => {
		expect(scrobbleManager.status).toBe('idle');
		expect(scrobbleManager.enabled).toBe(false);
	});

	describe('init', () => {
		beforeEach(() => {
			mockGet.mockReset();
			mockPost.mockReset();
		});

		it('enables when lastfm scrobbling is on', async () => {
			mockGet.mockResolvedValueOnce({ scrobble_to_lastfm: true, scrobble_to_listenbrainz: false });
			await scrobbleManager.refreshSettings();
			expect(scrobbleManager.enabled).toBe(true);
		});

		it('enables when listenbrainz scrobbling is on', async () => {
			mockGet.mockResolvedValueOnce({ scrobble_to_lastfm: false, scrobble_to_listenbrainz: true });
			await scrobbleManager.refreshSettings();
			expect(scrobbleManager.enabled).toBe(true);
		});

		it('disables when both are off', async () => {
			mockGet.mockResolvedValueOnce({ scrobble_to_lastfm: false, scrobble_to_listenbrainz: false });
			await scrobbleManager.refreshSettings();
			expect(scrobbleManager.enabled).toBe(false);
		});

		it('keeps prior state on fetch failure during init', async () => {
			mockGet.mockResolvedValueOnce({ scrobble_to_lastfm: true, scrobble_to_listenbrainz: false });
			await scrobbleManager.refreshSettings();
			expect(scrobbleManager.enabled).toBe(true);

			mockGet.mockRejectedValueOnce(new Error('network'));
			await scrobbleManager.init();
			expect(scrobbleManager.enabled).toBe(true);
		});
	});

	describe('refreshSettings', () => {
		beforeEach(() => {
			mockGet.mockReset();
			mockPost.mockReset();
		});

		it('clears cache and re-fetches', async () => {
			mockGet
				.mockResolvedValueOnce({ scrobble_to_lastfm: true, scrobble_to_listenbrainz: false })
				.mockResolvedValueOnce({ scrobble_to_lastfm: false, scrobble_to_listenbrainz: false });
			await scrobbleManager.refreshSettings();
			expect(scrobbleManager.enabled).toBe(true);

			await scrobbleManager.refreshSettings();
			expect(scrobbleManager.enabled).toBe(false);
			expect(mockGet).toHaveBeenCalledTimes(2);
		});
	});
});

describe('makeTrackKey', () => {
	it('lowercases and joins artist and track', () => {
		expect.assertions(1);
		expect(makeTrackKey('Radiohead', 'Creep')).toBe('radiohead::creep');
	});

	it('produces different keys for different tracks', () => {
		expect.assertions(1);
		expect(makeTrackKey('Muse', 'Hysteria')).not.toBe(makeTrackKey('Muse', 'Starlight'));
	});
});

describe('shouldAccumulate (seek inflation protection)', () => {
	it('accumulates for normal 1-second playback delta', () => {
		expect.assertions(1);
		expect(shouldAccumulate(1)).toBe(true);
	});

	it('accumulates for small positive delta under tolerance', () => {
		expect.assertions(1);
		expect(shouldAccumulate(2.5)).toBe(true);
	});

	it('rejects zero delta', () => {
		expect.assertions(1);
		expect(shouldAccumulate(0)).toBe(false);
	});

	it('rejects negative delta (seek backward)', () => {
		expect.assertions(1);
		expect(shouldAccumulate(-5)).toBe(false);
	});

	it('rejects large forward seek (above tolerance)', () => {
		expect.assertions(1);
		expect(shouldAccumulate(10)).toBe(false);
	});

	it('rejects delta exactly at tolerance boundary', () => {
		expect.assertions(1);
		expect(shouldAccumulate(3)).toBe(false);
	});
});

describe('isLoopReset', () => {
	const DURATION_MS = 180_000; // 3 minutes = 180s

	it('detects loop when progress resets from near end to near start', () => {
		expect.assertions(1);
		expect(isLoopReset(179, 0.5, DURATION_MS)).toBe(true);
	});

	it('does not trigger when previous progress is mid-track', () => {
		expect.assertions(1);
		expect(isLoopReset(90, 0.5, DURATION_MS)).toBe(false);
	});

	it('does not trigger when current progress is past tolerance from start', () => {
		expect.assertions(1);
		expect(isLoopReset(179, 5, DURATION_MS)).toBe(false);
	});

	it('does not trigger when duration is zero', () => {
		expect.assertions(1);
		expect(isLoopReset(179, 0.5, 0)).toBe(false);
	});

	it('does not trigger when previous progress is zero', () => {
		expect.assertions(1);
		expect(isLoopReset(0, 0.5, DURATION_MS)).toBe(false);
	});

	it('triggers at exact boundary values', () => {
		expect.assertions(1);
		const durationS = DURATION_MS / 1000;
		const prevAtBoundary = durationS - LOOP_RESET_TOLERANCE_S;
		expect(isLoopReset(prevAtBoundary, LOOP_RESET_TOLERANCE_S - 0.01, DURATION_MS)).toBe(true);
	});
});

describe('shouldSendNowPlaying (debounce)', () => {
	it('returns false when already sent', () => {
		expect.assertions(1);
		expect(shouldSendNowPlaying(NOW_PLAYING_DEBOUNCE_MS + 1000, true)).toBe(false);
	});

	it('returns false when accumulated time is below debounce', () => {
		expect.assertions(1);
		expect(shouldSendNowPlaying(NOW_PLAYING_DEBOUNCE_MS - 1, false)).toBe(false);
	});

	it('returns true when accumulated time meets debounce and not yet sent', () => {
		expect.assertions(1);
		expect(shouldSendNowPlaying(NOW_PLAYING_DEBOUNCE_MS, false)).toBe(true);
	});

	it('returns true when accumulated time exceeds debounce and not yet sent', () => {
		expect.assertions(1);
		expect(shouldSendNowPlaying(NOW_PLAYING_DEBOUNCE_MS + 5000, false)).toBe(true);
	});
});

describe('getScrobbleThreshold (50% / 4min rule)', () => {
	it('uses 50% of a 5-minute track (150s < 240s)', () => {
		expect.assertions(1);
		const durationMs = 300_000; // 5 min
		expect(getScrobbleThreshold(durationMs)).toBe(150_000);
	});

	it('uses 4 minutes for a 10-minute track (300s > 240s)', () => {
		expect.assertions(1);
		const durationMs = 600_000; // 10 min
		expect(getScrobbleThreshold(durationMs)).toBe(SCROBBLE_TIME_THRESHOLD_MS);
	});

	it('uses 50% for exactly 8-minute track (240s == 240s)', () => {
		expect.assertions(1);
		const durationMs = 480_000; // 8 min → 50% = 240s
		expect(getScrobbleThreshold(durationMs)).toBe(SCROBBLE_TIME_THRESHOLD_MS);
	});

	it('uses 50% for short 1-minute track', () => {
		expect.assertions(1);
		const durationMs = 60_000;
		expect(getScrobbleThreshold(durationMs)).toBe(30_000);
	});
});

describe('shouldScrobble (threshold enforcement)', () => {
	it('returns true when 50% threshold met for 5-minute track', () => {
		expect.assertions(1);
		const durationMs = 300_000;
		const threshold = 150_000; // 50% of 5 min
		expect(shouldScrobble(threshold, durationMs, false)).toBe(true);
	});

	it('returns false before threshold is met', () => {
		expect.assertions(1);
		const durationMs = 300_000;
		expect(shouldScrobble(149_999, durationMs, false)).toBe(false);
	});

	it('returns false when already scrobbled', () => {
		expect.assertions(1);
		const durationMs = 300_000;
		expect(shouldScrobble(200_000, durationMs, true)).toBe(false);
	});

	it('returns false for tracks shorter than 30 seconds', () => {
		expect.assertions(1);
		const durationMs = 25_000;
		expect(shouldScrobble(25_000, durationMs, false)).toBe(false);
	});

	it('returns false for tracks exactly at minimum duration boundary', () => {
		expect.assertions(1);
		const durationMs = MIN_TRACK_DURATION_MS - 1;
		expect(shouldScrobble(durationMs, durationMs, false)).toBe(false);
	});

	it('uses 4-minute cap for long tracks', () => {
		expect.assertions(2);
		const durationMs = 600_000; // 10 min
		expect(shouldScrobble(SCROBBLE_TIME_THRESHOLD_MS - 1, durationMs, false)).toBe(false);
		expect(shouldScrobble(SCROBBLE_TIME_THRESHOLD_MS, durationMs, false)).toBe(true);
	});

	it('scrobbles track at minimum accepted duration (30s)', () => {
		expect.assertions(1);
		const durationMs = MIN_TRACK_DURATION_MS; // exactly 30s
		const threshold = durationMs * SCROBBLE_PERCENT_THRESHOLD; // 15s
		expect(shouldScrobble(threshold, durationMs, false)).toBe(true);
	});
});

describe('seek inflation protection (integration)', () => {
	it('seeking past 50% does not count toward scrobble', () => {
		expect.assertions(1);
		let accumulated = 0;
		// Simulate 10 seconds of normal play
		for (let i = 0; i < 10; i++) {
			const delta = 1;
			if (shouldAccumulate(delta)) accumulated += delta * 1000;
		}
		// Then seek forward 100 seconds
		const seekDelta = 100;
		if (shouldAccumulate(seekDelta)) accumulated += seekDelta * 1000;
		// Only 10 seconds should be accumulated
		expect(accumulated).toBe(10_000);
	});

	it('seeking backward does not add time', () => {
		expect.assertions(1);
		let accumulated = 0;
		for (let i = 0; i < 5; i++) {
			if (shouldAccumulate(1)) accumulated += 1000;
		}
		if (shouldAccumulate(-30)) accumulated += 30_000; // should not execute
		expect(accumulated).toBe(5000);
	});
});

describe('track change reset', () => {
	it('different tracks produce different keys', () => {
		expect.assertions(1);
		const key1 = makeTrackKey('Artist', 'Track A');
		const key2 = makeTrackKey('Artist', 'Track B');
		expect(key1).not.toBe(key2);
	});

	it('same track by same artist produces the same key', () => {
		expect.assertions(1);
		expect(makeTrackKey('MUSE', 'Hysteria')).toBe(makeTrackKey('muse', 'hysteria'));
	});
});

describe('missing metadata handling', () => {
	it('makeTrackKey works with empty strings', () => {
		expect.assertions(1);
		expect(makeTrackKey('', '')).toBe('::');
	});
});
