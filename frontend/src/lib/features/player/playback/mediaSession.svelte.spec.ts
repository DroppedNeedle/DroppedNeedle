import { describe, expect, it, vi } from 'vitest';

const env = vi.hoisted(() => ({ apiUrl: undefined as string | undefined }));
vi.mock('$lib/api/api-utils', async (actual) => ({
	...(await actual<typeof import('$lib/api/api-utils')>()),
	apiOrigin: () => env.apiUrl
}));

import { flushSync } from 'svelte';
import { coverSrc } from '$lib/api/covers';
import { startMediaSession, type MediaSessionPlayer } from './mediaSession.svelte';
import type { NowPlaying, PlaybackState } from './types';

const MBID = '0b1f2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d';

function song(overrides: Partial<NowPlaying> = {}): NowPlaying {
	return {
		albumId: MBID,
		albumName: 'Album',
		artistName: 'Artist',
		coverUrl: null,
		sourceType: 'local',
		trackName: 'Song',
		...overrides
	};
}

function fakePlayer() {
	let nowPlaying = $state<NowPlaying | null>(null);
	let playbackState = $state<PlaybackState>('idle');
	let progress = $state(0);
	let duration = $state(0);
	let isSeekable = $state(true);
	const actions = {
		play: vi.fn(),
		pause: vi.fn(),
		nextTrack: vi.fn(),
		previousTrack: vi.fn(),
		seekTo: vi.fn()
	};
	const player: MediaSessionPlayer & {
		set(p: {
			nowPlaying?: NowPlaying | null;
			playbackState?: PlaybackState;
			progress?: number;
			duration?: number;
			isSeekable?: boolean;
		}): void;
	} = {
		get nowPlaying() {
			return nowPlaying;
		},
		get playbackState() {
			return playbackState;
		},
		get progress() {
			return progress;
		},
		get duration() {
			return duration;
		},
		get isSeekable() {
			return isSeekable;
		},
		...actions,
		set(p) {
			if (p.nowPlaying !== undefined) nowPlaying = p.nowPlaying;
			if (p.playbackState !== undefined) playbackState = p.playbackState;
			if (p.progress !== undefined) progress = p.progress;
			if (p.duration !== undefined) duration = p.duration;
			if (p.isSeekable !== undefined) isSeekable = p.isSeekable;
			flushSync();
		}
	};
	return { player, actions };
}

function fakeSession() {
	const handlers = new Map<MediaSessionAction, MediaSessionActionHandler>();
	const positions: (MediaPositionState | undefined)[] = [];
	const session = {
		metadata: null as MediaMetadata | null,
		playbackState: 'none' as MediaSessionPlaybackState,
		setActionHandler: vi.fn(
			(action: MediaSessionAction, handler: MediaSessionActionHandler | null) => {
				if (handler) handlers.set(action, handler);
				else handlers.delete(action);
			}
		),
		setPositionState: vi.fn((state?: MediaPositionState) => {
			positions.push(state);
		})
	};
	const fire = (action: MediaSessionAction, details: Partial<MediaSessionActionDetails> = {}) =>
		handlers.get(action)?.({ action, ...details } as MediaSessionActionDetails);
	return { session: session as unknown as MediaSession, raw: session, handlers, positions, fire };
}

describe('media session', () => {
	it('shows the song and its cover when the track changes, and clears it all when playback stops', () => {
		const { player } = fakePlayer();
		const fake = fakeSession();
		const stop = startMediaSession(player, fake.session, () => 0);

		player.set({ nowPlaying: song(), playbackState: 'playing' });
		expect(fake.raw.metadata?.title).toBe('Song');
		expect(fake.raw.metadata?.artist).toBe('Artist');
		expect(fake.raw.metadata?.album).toBe('Album');
		expect(fake.raw.metadata?.artwork.map((a) => a.sizes)).toEqual(['250x250', '500x500']);
		expect(fake.raw.metadata?.artwork[0].src).toBe(
			new URL(coverSrc('release-group', MBID, 250), location.href).href
		);
		expect(fake.raw.playbackState).toBe('playing');

		player.set({
			nowPlaying: song({
				albumId: 'local-1',
				trackName: 'Next',
				coverUrl: '/api/v3/library/albums/local-1/artwork'
			})
		});
		expect(fake.raw.metadata?.title).toBe('Next');
		expect(fake.raw.metadata?.artwork.map((a) => new URL(a.src).pathname)).toEqual([
			'/api/v3/library/albums/local-1/artwork'
		]);

		// An address that is neither ours nor https is never handed to the system,
		// and one that does not parse leaves the song shown without a cover.
		for (const coverUrl of ['javascript:alert(1)', 'http://elsewhere.test/c.jpg', 'https://[bad']) {
			player.set({ nowPlaying: song({ albumId: 'x', coverUrl }) });
			expect(fake.raw.metadata?.title).toBe('Song');
			expect(fake.raw.metadata?.artwork).toEqual([]);
		}

		// The configured API origin is ours, on http too.
		env.apiUrl = 'http://api.test:8080';
		player.set({ nowPlaying: song({ albumId: 'y', coverUrl: 'http://api.test:8080/c.jpg' }) });
		expect(fake.raw.metadata?.artwork.map((a) => a.src)).toEqual(['http://api.test:8080/c.jpg']);
		env.apiUrl = undefined;

		player.set({ playbackState: 'paused' });
		expect(fake.raw.playbackState).toBe('paused');

		player.set({ nowPlaying: null, playbackState: 'idle' });
		expect(fake.raw.metadata).toBeNull();
		expect(fake.raw.playbackState).toBe('none');
		expect(fake.handlers.size).toBe(0);

		player.set({ nowPlaying: song() });
		expect([...fake.handlers.keys()].sort()).toEqual([
			'nexttrack',
			'pause',
			'play',
			'previoustrack',
			'seekto'
		]);
		stop();
		expect(fake.raw.metadata).toBeNull();
		expect(fake.handlers.size).toBe(0);
	});

	it('tells the position clamped to the length, on changes and seeks but not every frame', () => {
		const { player } = fakePlayer();
		const fake = fakeSession();
		let clock = 0;
		const stop = startMediaSession(player, fake.session, () => clock);

		// No length yet: nothing a system could show.
		player.set({ nowPlaying: song(), playbackState: 'loading', progress: 0, duration: NaN });
		expect(fake.positions).toEqual([]);

		player.set({ playbackState: 'playing', duration: 200 });
		expect(fake.positions.at(-1)).toEqual({ duration: 200, playbackRate: 1, position: 0 });

		// Listening along: progress a few times a second tells nothing new.
		const told = fake.positions.length;
		for (let t = 0.25; t <= 3; t += 0.25) {
			clock = t * 1000;
			player.set({ progress: t });
		}
		expect(fake.positions.length).toBe(told);

		// A seek tells at once.
		clock = 3250;
		player.set({ progress: 120 });
		expect(fake.positions.at(-1)).toEqual({ duration: 200, playbackRate: 1, position: 120 });

		// Past the end and garbage are held inside the song.
		player.set({ progress: 250 });
		expect(fake.positions.at(-1)?.position).toBe(200);
		player.set({ progress: NaN });
		expect(fake.positions.at(-1)?.position).toBe(0);

		// A pause tells where it stopped.
		player.set({ playbackState: 'paused', progress: 42 });
		expect(fake.positions.at(-1)).toEqual({ duration: 200, playbackRate: 1, position: 42 });
		stop();
	});

	it("hands each button to the player's own action", () => {
		const { player, actions } = fakePlayer();
		const fake = fakeSession();
		const stop = startMediaSession(player, fake.session, () => 0);
		player.set({ nowPlaying: song(), playbackState: 'playing', progress: 30, duration: 200 });

		fake.fire('play');
		expect(actions.play).toHaveBeenCalledTimes(1);
		fake.fire('pause');
		expect(actions.pause).toHaveBeenCalledTimes(1);
		fake.fire('nexttrack');
		expect(actions.nextTrack).toHaveBeenCalledTimes(1);
		fake.fire('previoustrack');
		expect(actions.previousTrack).toHaveBeenCalledTimes(1);
		fake.fire('seekto', { seekTime: 90 });
		expect(actions.seekTo).toHaveBeenLastCalledWith(90);

		// A song that cannot seek offers no seeking.
		player.set({ isSeekable: false });
		expect(fake.handlers.has('seekto')).toBe(false);
		expect(fake.handlers.has('play')).toBe(true);
		stop();
	});
});
