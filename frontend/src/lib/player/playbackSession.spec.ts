import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const calls: Array<[string, unknown]> = [];

vi.mock('./playbackGateway', async (importActual) => ({
	...(await importActual<typeof import('./playbackGateway')>()),
	startPlaybackSession: vi.fn(async (body: unknown) => {
		calls.push(['start', body]);
		return { session: 's' };
	}),
	reportPlaybackProgress: vi.fn(async (body: unknown) => {
		calls.push(['progress', body]);
	}),
	stopPlaybackSession: vi.fn(async (body: unknown) => {
		calls.push(['stop', body]);
	})
}));

import type { QueueItem } from './types';
import { createPlaybackSession, sessionKeyFor } from './playbackSession';

function item(overrides: Partial<QueueItem>): QueueItem {
	return {
		trackSourceId: 'part-or-id',
		trackName: 'T',
		artistName: 'A',
		trackNumber: 1,
		albumId: 'al',
		albumName: 'Al',
		coverUrl: null,
		sourceType: 'local',
		...overrides
	};
}

// Plays one track: load, play past one report, pause, resume, leave it.
async function playThrough(track: QueueItem): Promise<Array<[string, unknown]>> {
	calls.length = 0;
	let seconds = 0;
	let isPaused = false;
	const session = createPlaybackSession({
		intervalMs: 10_000,
		maxFailures: 3,
		position: () => ({ seconds, isPaused })
	});
	await session.begin(sessionKeyFor(track));
	session.playing();
	seconds = 12;
	await vi.advanceTimersByTimeAsync(10_000);
	isPaused = true;
	session.paused(seconds);
	await vi.advanceTimersByTimeAsync(0);
	isPaused = false;
	session.playing();
	seconds = 30;
	await session.end(seconds);
	await vi.advanceTimersByTimeAsync(20_000);
	return calls.map(([kind, body]) => [kind, { ...(body as object), source: 'x', track_id: 'x' }]);
}

describe('playback session reporter', () => {
	beforeEach(() => {
		vi.useFakeTimers();
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it('reports a Jellyfin, a Navidrome and a Plex track with the same call sequence', async () => {
		const jellyfin = await playThrough(item({ sourceType: 'jellyfin', trackSourceId: 'jf-1' }));
		const navidrome = await playThrough(item({ sourceType: 'navidrome', trackSourceId: 'nd-1' }));
		const plex = await playThrough(item({ sourceType: 'plex', plexRatingKey: 'rk-1' }));

		expect(jellyfin).toEqual([
			['start', { source: 'x', track_id: 'x' }],
			['progress', { source: 'x', track_id: 'x', position_ms: 12_000, is_paused: false }],
			['progress', { source: 'x', track_id: 'x', position_ms: 12_000, is_paused: true }],
			['stop', { source: 'x', track_id: 'x', position_ms: 30_000 }]
		]);
		expect(navidrome).toEqual(jellyfin);
		expect(plex).toEqual(jellyfin);
	});

	it('keys each source by the id its server knows', () => {
		expect(sessionKeyFor(item({ sourceType: 'jellyfin', trackSourceId: 'jf-1' }))).toEqual({
			source: 'jellyfin',
			trackId: 'jf-1'
		});
		expect(sessionKeyFor(item({ sourceType: 'plex', plexRatingKey: 'rk-1' }))).toEqual({
			source: 'plex',
			trackId: 'rk-1'
		});
		expect(sessionKeyFor(item({ sourceType: 'local' }))).toBeNull();
		expect(sessionKeyFor(item({ sourceType: 'youtube' }))).toBeNull();
	});
});
