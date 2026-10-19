import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync } from 'svelte';
import type { NowPlaying } from '$lib/player/types';
import type { scrobbleManager as ScrobbleManager } from './scrobble.svelte';

const h = vi.hoisted(() => ({
	settings: vi.fn(),
	submit: vi.fn(),
	nowPlaying: vi.fn()
}));

const player = $state<{ now: NowPlaying | null; playing: boolean }>({ now: null, playing: false });
let progress = 0;

vi.mock('$lib/api/client', () => ({ api: { global: { v3: { GET: h.settings } } } }));
vi.mock('$lib/player/playbackGateway', () => ({
	submitScrobble: h.submit,
	sendScrobbleNowPlaying: h.nowPlaying
}));
vi.mock('$lib/utils/userSessionCleanup', () => ({ registerUserSessionReset: () => () => {} }));
vi.mock('$lib/stores/player.svelte', () => ({
	playerStore: {
		get nowPlaying() {
			return player.now;
		},
		get isPlaying() {
			return player.playing;
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

	it('drops the play when the user signs out mid-play', async () => {
		await listen(50);
		manager.reset();
		flushSync();
		await listen(150);
		expect(h.submit).not.toHaveBeenCalled();
	});
});
