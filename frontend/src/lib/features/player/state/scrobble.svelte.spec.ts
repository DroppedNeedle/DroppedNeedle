import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import type { NowPlaying } from '$lib/features/player/playback/types';
import type { LocalTrackInfo, PlayEnd, PlaybackState } from '$lib/features/player/playback/types';
import { buildQueueItemsFromLocal } from '$lib/features/player/playback/queueHelpers';
import { buildNowPlayingMetadata } from './playerSourceResolver';
import type { scrobbleManager as ScrobbleManager } from './scrobble.svelte';

const h = vi.hoisted(() => ({
	settings: vi.fn(),
	submit: vi.fn(),
	event: vi.fn(),
	nowPlaying: vi.fn()
}));

const player = $state<{ now: NowPlaying | null; playing: boolean; playId: number }>({
	now: null,
	playing: false,
	playId: 0
});
let progress = 0;
let playEnd: PlayEnd = 'stopped';
let playbackState: PlaybackState = 'playing';

vi.mock('$lib/api/client', () => ({ api: { v3: { GET: h.settings } } }));
vi.mock('$lib/features/player/playback/playbackGateway', () => ({
	submitScrobble: h.submit,
	reportPlaybackEvent: h.event,
	sendScrobbleNowPlaying: h.nowPlaying
}));
vi.mock('$lib/utils/userSessionCleanup', () => ({ registerUserSessionReset: () => () => {} }));
vi.mock('$lib/features/player/state/player.svelte', () => ({
	playerStore: {
		get nowPlaying() {
			return player.now;
		},
		get isPlaying() {
			return player.playing;
		},
		get playId() {
			return player.playId;
		},
		get playEnd() {
			return playEnd;
		},
		get playbackState() {
			return playbackState;
		},
		get progress() {
			return progress;
		},
		get duration() {
			return 200;
		}
	}
}));

const localPlay: NowPlaying = {
	albumId: 'album-1',
	albumName: 'Album',
	artistName: 'Artist',
	coverUrl: null,
	sourceType: 'local',
	trackSourceId: 'track-1',
	trackName: 'Song'
};

let manager: typeof ScrobbleManager;

async function listen(seconds: number): Promise<void> {
	for (let second = 0; second < seconds; second++) {
		progress += 1;
		await vi.advanceTimersByTimeAsync(1000);
	}
}

describe('web player plays with forwarding off', () => {
	beforeAll(async () => {
		// Imported here so the player mock's state exists before the manager's effect reads it.
		({ scrobbleManager: manager } = await import('./scrobble.svelte'));
	});

	beforeEach(async () => {
		vi.useFakeTimers();
		h.settings.mockResolvedValue({ scrobble_to_lastfm: false, scrobble_to_listenbrainz: false });
		h.submit.mockResolvedValue({ accepted: true, services: {} });
		await manager.refreshSettings();
		progress = 0;
		player.now = { ...localPlay };
		player.playing = true;
		flushSync();
	});

	afterEach(() => {
		player.now = null;
		player.playing = false;
		manager.reset();
		flushSync();
		vi.useRealTimers();
		vi.clearAllMocks();
	});

	it('records a local play past the threshold once, under its track', async () => {
		expect(manager.enabled).toBe(false);
		await listen(150);
		expect(h.submit).toHaveBeenCalledTimes(1);
		expect(h.submit.mock.calls[0][0]).toMatchObject({ track_id: 'track-1', source: 'local' });
		expect(h.nowPlaying).not.toHaveBeenCalled();
	});

	it('retries a failed submit three times with growing pauses, not every second', async () => {
		h.submit.mockRejectedValue(new Error('server unreachable'));
		await listen(200);
		expect(h.submit).toHaveBeenCalledTimes(4);
	});

	it('records the same song played again straight after as a second play', async () => {
		// A crossfade leaves the first play before its end, so only the player's new play tells them apart.
		await listen(150);
		player.playId += 1;
		progress = 0;
		flushSync();
		await listen(150);
		expect(h.submit).toHaveBeenCalledTimes(2);
	});

	it('drops the play when the user signs out mid-play', async () => {
		await listen(50);
		manager.reset();
		flushSync();
		await listen(150);
		expect(h.submit).not.toHaveBeenCalled();
	});
});

describe('how a web player play ended', () => {
	beforeAll(async () => {
		({ scrobbleManager: manager } = await import('./scrobble.svelte'));
	});

	beforeEach(async () => {
		vi.useFakeTimers();
		h.settings.mockResolvedValue({ scrobble_to_lastfm: false, scrobble_to_listenbrainz: false });
		h.submit.mockResolvedValue({ accepted: true, services: {} });
		h.event.mockResolvedValue(undefined);
		await manager.refreshSettings();
		progress = 0;
		playEnd = 'stopped';
		playbackState = 'playing';
	});

	afterEach(() => {
		player.now = null;
		player.playing = false;
		manager.reset();
		flushSync();
		vi.useRealTimers();
		vi.clearAllMocks();
	});

	function startPlay(now: NowPlaying): void {
		player.now = now;
		player.playing = true;
		flushSync();
	}

	/** The player moves on to another play, for this reason. */
	function moveOn(why: PlayEnd): void {
		playEnd = why;
		player.playId += 1;
		progress = 0;
		flushSync();
	}

	it('reports a track left at 20 seconds once, as skipped and not counted, with no scrobble', async () => {
		startPlay({ ...localPlay });
		await listen(20);
		moveOn('skipped');
		expect(h.submit).not.toHaveBeenCalled();
		expect(h.event).toHaveBeenCalledTimes(1);
		expect(h.event.mock.calls[0][0]).toMatchObject({
			track_id: 'track-1',
			source: 'local',
			listened_ms: 20_000,
			ended_by: 'skipped',
			came_from: 'unknown',
			counted: false
		});
	});

	it('scrobbles a track played through at the threshold, then reports it finished and counted', async () => {
		startPlay({ ...localPlay });
		await listen(99);
		expect(h.submit).not.toHaveBeenCalled();
		await listen(1);
		expect(h.submit).toHaveBeenCalledTimes(1);
		await listen(100);
		expect(h.event).not.toHaveBeenCalled();
		moveOn('finished');
		expect(h.submit).toHaveBeenCalledTimes(1);
		expect(h.event).toHaveBeenCalledTimes(1);
		expect(h.event.mock.calls[0][0]).toMatchObject({
			listened_ms: 200_000,
			duration_ms: 200_000,
			ended_by: 'finished',
			counted: true
		});
	});

	it('reports a play that failed as failed when the page is left', async () => {
		startPlay({ ...localPlay });
		await listen(20);
		playbackState = 'error';
		window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: false }));
		expect(h.event).toHaveBeenCalledTimes(1);
		expect(h.event.mock.calls[0][0]).toMatchObject({ listened_ms: 20_000, ended_by: 'failed' });
	});

	it('reports nothing for a preview', async () => {
		startPlay({ ...localPlay, isPreview: true });
		await listen(30);
		moveOn('finished');
		player.now = null;
		flushSync();
		expect(h.event).not.toHaveBeenCalled();
		expect(h.submit).not.toHaveBeenCalled();
	});

	it('names the album a queue was started from', async () => {
		const track: LocalTrackInfo = {
			track_file_id: 'track-1',
			title: 'Song',
			track_number: 1,
			size_bytes: 1,
			format: 'FLAC'
		};
		const [item] = buildQueueItemsFromLocal([track], {
			albumId: 'album-1',
			albumName: 'Album',
			artistName: 'Artist',
			coverUrl: null
		});
		startPlay(buildNowPlayingMetadata(item));
		await listen(5);
		moveOn('skipped');
		expect(h.event).toHaveBeenCalledTimes(1);
		expect(h.event.mock.calls[0][0]).toMatchObject({
			track_id: 'track-1',
			came_from: 'album',
			came_from_id: 'album-1'
		});
	});
});
