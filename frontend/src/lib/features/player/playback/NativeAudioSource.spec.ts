import { describe, expect, it, vi } from 'vitest';

const hoisted = vi.hoisted(() => {
	const listeners = new Map<string, Set<EventListener>>();
	const resumeAudioEngine = vi.fn(async () => undefined);
	const audio = {
		src: '',
		volume: 1,
		currentTime: 0,
		duration: 180,
		ended: false,
		paused: true,
		error: null as MediaError | null,
		play: vi.fn(() => Promise.resolve()),
		pause: vi.fn(),
		load: vi.fn(),
		addEventListener: vi.fn((event: string, handler: EventListener) => {
			const set = listeners.get(event) ?? new Set<EventListener>();
			set.add(handler);
			listeners.set(event, set);
		}),
		removeEventListener: vi.fn((event: string, handler: EventListener) => {
			listeners.get(event)?.delete(handler);
		})
	};

	const dispatch = (event: string): void => {
		for (const handler of listeners.get(event) ?? []) {
			handler(new Event(event));
		}
	};

	const reset = (): void => {
		listeners.clear();
		audio.src = '';
		audio.volume = 1;
		audio.currentTime = 0;
		audio.duration = 180;
		audio.ended = false;
		audio.paused = true;
		audio.error = null;
		audio.play.mockReset();
		audio.play.mockImplementation(() => Promise.resolve());
		audio.pause.mockReset();
		audio.load.mockReset();
		audio.addEventListener.mockClear();
		audio.removeEventListener.mockClear();
		resumeAudioEngine.mockReset();
		resumeAudioEngine.mockResolvedValue(undefined);
	};

	return { audio, dispatch, reset, resumeAudioEngine };
});

vi.mock('./audioElement', () => ({
	resumeAudioEngine: hoisted.resumeAudioEngine,
	tryGetAudioEngine: () => null
}));

import { NativeAudioSource } from './NativeAudioSource';
import type { DeckLease } from './audioElement';

const deck: DeckLease = {
	element: hoisted.audio as unknown as HTMLAudioElement,
	deck: 0,
	held: () => true,
	played: () => {},
	release: () => {}
};

function fresh(): void {
	hoisted.reset();
	vi.useRealTimers();
}

describe('NativeAudioSource', () => {
	it('loads on canplay, and fails on a timeout, a blocked autoplay or a media error, then plays after a seek', async () => {
		{
			// Loads successfully on canplay.
			fresh();
			const source = new NativeAudioSource('local', { deck, url: '/audio.mp3', seekable: true });
			const loadPromise = source.load();

			expect(hoisted.audio.src).toBe('/audio.mp3');
			hoisted.dispatch('canplay');

			await expect(loadPromise).resolves.toBeUndefined();
		}

		{
			// Fails load when timeout is reached.
			fresh();
			vi.useFakeTimers();
			const source = new NativeAudioSource('local', { deck, url: '/timeout.mp3', seekable: true });
			const loadPromise = source.load();

			vi.advanceTimersByTime(15_000);

			await expect(loadPromise).rejects.toThrow('load timed out');
		}

		{
			// Reports autoplay blocked when play promise rejects.
			fresh();
			const source = new NativeAudioSource('local', { deck, url: '/blocked.mp3', seekable: true });
			const onError = vi.fn();
			source.onError(onError);

			hoisted.audio.play.mockImplementationOnce(() =>
				Promise.reject(new DOMException('blocked', 'NotAllowedError'))
			);
			source.play();
			await Promise.resolve();
			await Promise.resolve();

			expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'AUTOPLAY_BLOCKED' }));
		}

		{
			// Rejects load promise on media error event.
			fresh();
			const source = new NativeAudioSource('local', { deck, url: '/bad.mp3', seekable: true });
			const onError = vi.fn();
			source.onError(onError);

			const loadPromise = source.load();

			hoisted.audio.error = { code: 4 } as MediaError;
			hoisted.dispatch('error');

			await expect(loadPromise).rejects.toThrow('MEDIA_ERR_SRC_NOT_SUPPORTED');
			expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'UNPLAYABLE' }));
		}

		{
			// Transitions from buffering back to playing after seek via playing event.
			fresh();
			const source = new NativeAudioSource('local', { deck, url: '/seek.mp3', seekable: true });
			const states: string[] = [];
			source.onStateChange((s) => states.push(s));

			const loadPromise = source.load();
			hoisted.dispatch('canplay');
			await loadPromise;

			// Asked to play is not playing: a file that cannot decode gets this far.
			hoisted.dispatch('play');
			expect(states).not.toContain('playing');

			hoisted.dispatch('playing');
			expect(states).toContain('playing');

			hoisted.dispatch('waiting');
			expect(states.at(-1)).toBe('buffering');

			hoisted.dispatch('playing');
			expect(states.at(-1)).toBe('playing');
		}
	});

	it('claims a playback audio session when a song starts', async () => {
		fresh();
		const audioSession = { type: 'auto' };
		vi.stubGlobal('navigator', { audioSession });
		const source = new NativeAudioSource('local', { deck, url: '/a.mp3', seekable: true });
		expect(audioSession.type).toBe('auto');
		source.play();
		await Promise.resolve();
		expect(audioSession.type).toBe('playback');
		vi.unstubAllGlobals();
	});

	it('never counts a paused deck as stalled, so nothing plays it again', async () => {
		fresh();
		vi.useFakeTimers();
		const source = new NativeAudioSource('local', { deck, url: '/a.mp3', seekable: true });
		const onError = vi.fn();
		source.onError(onError);
		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;
		source.play();
		hoisted.audio.paused = false;
		hoisted.dispatch('playing');

		// Paused while waiting for data, then iOS stops fetching the paused stream.
		hoisted.dispatch('waiting');
		hoisted.audio.paused = true;
		hoisted.dispatch('pause');
		hoisted.dispatch('stalled');
		vi.advanceTimersByTime(60_000);

		expect(onError).not.toHaveBeenCalled();
		expect(hoisted.audio.play).toHaveBeenCalledTimes(1);

		// Playing again and starved of data for 15 seconds is a stall.
		hoisted.audio.paused = false;
		hoisted.dispatch('playing');
		hoisted.dispatch('waiting');
		vi.advanceTimersByTime(15_000);
		expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'NETWORK_STALL' }));
		vi.useRealTimers();
	});
});
