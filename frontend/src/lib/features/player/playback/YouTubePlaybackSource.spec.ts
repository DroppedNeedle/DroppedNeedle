import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { YouTubePlaybackSource } from './YouTubePlaybackSource';

/**
 * YouTube's Developer Policies prohibit background play. The guard lives in this source
 * alone, so local files and the media-server sources keep playing when the page is hidden.
 */

const PLAYING = 1;
const PAUSED = 2;

let playerState = PLAYING;
let visibilityListeners: (() => void)[] = [];

const player = {
	playVideo: vi.fn(() => {
		playerState = PLAYING;
	}),
	pauseVideo: vi.fn(() => {
		playerState = PAUSED;
	}),
	getPlayerState: () => playerState,
	setVolume: vi.fn(),
	seekTo: vi.fn(),
	getCurrentTime: () => 0,
	getDuration: () => 0,
	destroy: vi.fn()
};

let lastPlayerVars: Record<string, unknown> = {};

function setVisibility(state: 'visible' | 'hidden'): void {
	(globalThis.document as unknown as { visibilityState: string }).visibilityState = state;
	for (const listener of [...visibilityListeners]) listener();
}

async function loadSource(): Promise<YouTubePlaybackSource> {
	const source = new YouTubePlaybackSource('yt-player-embed');
	await source.load({ trackSourceId: 'abc123' });
	return source;
}

beforeEach(() => {
	playerState = PLAYING;
	visibilityListeners = [];
	lastPlayerVars = {};
	vi.clearAllMocks();

	vi.stubGlobal('document', {
		visibilityState: 'visible',
		getElementById: () => ({}),
		querySelector: () => ({}),
		createElement: () => ({}),
		head: { appendChild: vi.fn() },
		addEventListener: (event: string, handler: () => void) => {
			if (event === 'visibilitychange') visibilityListeners.push(handler);
		},
		removeEventListener: (event: string, handler: () => void) => {
			if (event === 'visibilitychange') {
				visibilityListeners = visibilityListeners.filter((h) => h !== handler);
			}
		}
	});

	vi.stubGlobal('window', {
		YT: {
			Player: vi.fn(function (
				_id: string,
				options: { playerVars: Record<string, unknown>; events: { onReady: () => void } }
			) {
				lastPlayerVars = options.playerVars;
				queueMicrotask(() => options.events.onReady());
				return player;
			})
		}
	});
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('YouTubePlaybackSource background-play guard', () => {
	it('pauses when the page is hidden while playing', async () => {
		const source = await loadSource();

		setVisibility('hidden');

		expect(player.pauseVideo).toHaveBeenCalledTimes(1);
		source.destroy();
	});

	it('resumes when the page becomes visible again', async () => {
		const source = await loadSource();

		setVisibility('hidden');
		setVisibility('visible');

		expect(player.playVideo).toHaveBeenCalledTimes(1);
		source.destroy();
	});

	it('does not resume a track the user paused themselves', async () => {
		const source = await loadSource();

		source.pause();
		setVisibility('hidden');
		setVisibility('visible');

		expect(player.playVideo).not.toHaveBeenCalled();
		source.destroy();
	});

	it('never autoplays when the page is already hidden at load', async () => {
		(globalThis.document as unknown as { visibilityState: string }).visibilityState = 'hidden';

		const source = await loadSource();

		expect(lastPlayerVars.autoplay).toBe(0);
		source.destroy();
	});

	it('refuses a play request issued while the page is hidden', async () => {
		const source = await loadSource();

		setVisibility('hidden');
		vi.clearAllMocks();
		source.play();

		expect(player.playVideo).not.toHaveBeenCalled();

		setVisibility('visible');
		expect(player.playVideo).toHaveBeenCalledTimes(1);
		source.destroy();
	});

	it('removes its listener on destroy', async () => {
		const source = await loadSource();
		expect(visibilityListeners).toHaveLength(1);

		source.destroy();

		expect(visibilityListeners).toHaveLength(0);
	});
});
