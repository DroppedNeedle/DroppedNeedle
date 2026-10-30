import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { fakeApi } from '$lib/test/api';
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

// Plays one track as the player does: its length is not known when it
// loads, and is forgotten again before the stop of a track change.
async function playThrough(track: QueueItem): Promise<Array<[string, unknown]>> {
	const api = fakeApi({
		'POST /api/v3/playback/start': { accepted: true, session: 's' },
		'POST /api/v3/playback/progress': { accepted: true },
		'POST /api/v3/playback/stop': { accepted: true, scrobbled: false }
	});
	let seconds = 0;
	let length = 0;
	let isPaused = false;
	const session = createPlaybackSession({
		intervalMs: 10_000,
		maxFailures: 3,
		position: () => ({ seconds, isPaused }),
		duration: () => length
	});
	await session.begin(sessionKeyFor(track));
	length = 200;
	session.playing();
	seconds = 12;
	const reports = sessionKeyFor(track) ? 1 : 0;
	const progressSent = (count: number) =>
		vi.waitFor(() =>
			expect(api.calls('POST /api/v3/playback/progress')).toHaveLength(count * reports)
		);
	await vi.advanceTimersByTimeAsync(10_000);
	await progressSent(1);
	isPaused = true;
	session.paused(seconds);
	await progressSent(2);
	isPaused = false;
	session.playing();
	seconds = 30;
	length = 0;
	await session.end(seconds);
	await vi.advanceTimersByTimeAsync(20_000);
	return api.calls().map((call) => [call.url.pathname.split('/').pop() ?? '', call.body]);
}

describe('playback session reporter', () => {
	beforeEach(() => {
		vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
	});
	afterEach(() => {
		vi.useRealTimers();
	});

	it.each([
		['jellyfin', item({ sourceType: 'jellyfin', trackSourceId: 'jf-1' }), 'jf-1'],
		['navidrome', item({ sourceType: 'navidrome', trackSourceId: 'nd-1' }), 'nd-1'],
		['plex', item({ sourceType: 'plex', plexRatingKey: 'rk-1' }), 'rk-1']
	])('reports a %s track under the id its server knows', async (source, track, id) => {
		const sent = await playThrough(track);
		const play = { source, track_id: id };
		expect(sent).toEqual([
			['start', { ...play, position_ms: 0, duration_ms: 200_000 }],
			['progress', { ...play, position_ms: 12_000, duration_ms: 200_000, is_paused: false }],
			['progress', { ...play, position_ms: 12_000, duration_ms: 200_000, is_paused: true }],
			['stop', { ...play, position_ms: 30_000, duration_ms: 200_000 }]
		]);
	});

	it('reports nothing for a restored track that is never played', async () => {
		const api = fakeApi({});
		const beacon = vi.spyOn(navigator, 'sendBeacon').mockReturnValue(true);
		const session = createPlaybackSession({
			intervalMs: 10_000,
			maxFailures: 3,
			position: () => ({ seconds: 180, isPaused: true }),
			duration: () => 300
		});
		await session.begin(sessionKeyFor(item({ sourceType: 'navidrome', trackSourceId: 'nd-1' })));
		window.dispatchEvent(new Event('beforeunload'));
		await session.end(180);
		await vi.advanceTimersByTimeAsync(20_000);
		expect(api.calls()).toEqual([]);
		expect(beacon).not.toHaveBeenCalled();
		beacon.mockRestore();
	});

	it('reports nothing for a library or YouTube track', async () => {
		expect(await playThrough(item({ sourceType: 'local' }))).toEqual([]);
		expect(await playThrough(item({ sourceType: 'youtube' }))).toEqual([]);
		expect(sessionKeyFor(item({ sourceType: 'plex' }))).toBeNull();
	});
});
