import { beforeEach, describe, expect, it, vi } from 'vitest';

const hoisted = vi.hoisted(() => {
	const listeners = new Map<string, Set<EventListener>>();
	const resumeAudioEngine = vi.fn(async () => undefined);
	const audio = {
		src: '',
		volume: 1,
		currentTime: 0,
		duration: 180,
		ended: false,
		error: null as MediaError | null,
		play: vi.fn(() => Promise.resolve()),
		pause: vi.fn(),
		load: vi.fn(),
		removeAttribute: vi.fn(),
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
		audio.error = null;
		audio.play.mockReset();
		audio.play.mockImplementation(() => Promise.resolve());
		audio.pause.mockReset();
		audio.load.mockReset();
		audio.removeAttribute.mockReset();
		audio.addEventListener.mockClear();
		audio.removeEventListener.mockClear();
		resumeAudioEngine.mockReset();
		resumeAudioEngine.mockResolvedValue(undefined);
	};

	return {
		audio,
		dispatch,
		reset,
		getAudioElement: vi.fn(() => audio as unknown as HTMLAudioElement),
		resumeAudioEngine
	};
});

vi.mock('./audioElement', () => ({
	getAudioElement: hoisted.getAudioElement,
	resumeAudioEngine: hoisted.resumeAudioEngine
}));

import { NativeAudioSource } from './NativeAudioSource';

describe('NativeAudioSource', () => {
	beforeEach(() => {
		hoisted.reset();
		hoisted.getAudioElement.mockImplementation(() => hoisted.audio as unknown as HTMLAudioElement);
		vi.useRealTimers();
	});

	it('loads successfully on canplay', async () => {
		const source = new NativeAudioSource('local', { url: '/audio.mp3', seekable: true });
		const loadPromise = source.load();

		expect(hoisted.audio.src).toBe('/audio.mp3');
		hoisted.dispatch('canplay');

		await expect(loadPromise).resolves.toBeUndefined();
	});

	it('loads successfully on metadata and reports duration before playback timeupdates', async () => {
		const source = new NativeAudioSource('local', { url: '/metadata.mp3', seekable: true });
		const onProgress = vi.fn();
		source.onProgress(onProgress);
		const loadPromise = source.load();

		hoisted.audio.duration = 178;
		hoisted.dispatch('loadedmetadata');

		await expect(loadPromise).resolves.toBeUndefined();
		expect(onProgress).toHaveBeenCalledWith(0, 178);
	});

	it('detaches ready listeners on first ready event so onReady is single-shot', async () => {
		const source = new NativeAudioSource('local', { url: '/single-shot.mp3', seekable: true });
		const onReady = vi.fn();
		source.onReady(onReady);

		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		expect(onReady).toHaveBeenCalledTimes(1);
		for (const event of ['canplay', 'loadedmetadata', 'loadeddata']) {
			expect(hoisted.audio.removeEventListener).toHaveBeenCalledWith(event, expect.any(Function));
		}

		hoisted.dispatch('loadedmetadata');
		hoisted.dispatch('loadeddata');
		expect(onReady).toHaveBeenCalledTimes(1);
	});

	it('fails load when timeout is reached', async () => {
		vi.useFakeTimers();
		const source = new NativeAudioSource('local', { url: '/timeout.mp3', seekable: true });
		const loadPromise = source.load();

		vi.advanceTimersByTime(15_000);

		await expect(loadPromise).rejects.toThrow('load timed out');
	});

	it('emits network stall error after stalled timeout', async () => {
		vi.useFakeTimers();
		const source = new NativeAudioSource('local', { url: '/stall.mp3', seekable: true });
		const onError = vi.fn();
		source.onError(onError);

		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		hoisted.dispatch('stalled');
		vi.advanceTimersByTime(15_000);

		expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'NETWORK_STALL' }));
	});

	it('reports autoplay blocked when play promise rejects', async () => {
		const source = new NativeAudioSource('local', { url: '/blocked.mp3', seekable: true });
		const onError = vi.fn();
		source.onError(onError);

		hoisted.audio.play.mockImplementationOnce(() => Promise.reject(new Error('blocked')));
		source.play();
		await Promise.resolve();
		await Promise.resolve();

		expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'AUTOPLAY_BLOCKED' }));
	});

	it('resumes the Web Audio engine before native playback', async () => {
		const source = new NativeAudioSource('local', { url: '/resume.mp3', seekable: true });

		source.play();
		await Promise.resolve();
		await Promise.resolve();

		expect(hoisted.resumeAudioEngine).toHaveBeenCalledTimes(1);
		expect(hoisted.audio.play).toHaveBeenCalledTimes(1);
		expect(hoisted.resumeAudioEngine.mock.invocationCallOrder[0]).toBeLessThan(
			hoisted.audio.play.mock.invocationCallOrder[0]
		);
	});

	it('still attempts native playback if Web Audio resume rejects', async () => {
		const source = new NativeAudioSource('local', { url: '/resume-rejected.mp3', seekable: true });

		hoisted.resumeAudioEngine.mockRejectedValueOnce(new Error('resume blocked'));
		source.play();
		await Promise.resolve();
		await Promise.resolve();

		expect(hoisted.audio.play).toHaveBeenCalledTimes(1);
	});

	it('seekTo updates currentTime when stream is seekable', () => {
		const source = new NativeAudioSource('local', { url: '/seek.mp3', seekable: true });

		source.seekTo(42);

		expect(hoisted.audio.currentTime).toBe(42);
	});

	it('seekTo is no-op when stream is not seekable', () => {
		const source = new NativeAudioSource('jellyfin', { url: '/transcode.opus', seekable: false });
		hoisted.audio.currentTime = 5;

		source.seekTo(60);

		expect(hoisted.audio.currentTime).toBe(5);
	});

	it('destroy clears src and removes listeners', async () => {
		const source = new NativeAudioSource('local', { url: '/destroy.mp3', seekable: true });
		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		source.destroy();

		expect(hoisted.audio.src).toBe('');
		expect(hoisted.audio.removeEventListener).toHaveBeenCalled();
	});

	it('destroy pauses the element and drops the src attribute so nothing can restart it', async () => {
		const source = new NativeAudioSource('local', { url: '/disarm.mp3', seekable: true });
		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		source.destroy();

		expect(hoisted.audio.pause).toHaveBeenCalled();
		expect(hoisted.audio.removeAttribute).toHaveBeenCalledWith('src');
		expect(hoisted.audio.load).toHaveBeenCalledTimes(2); // initial load + reset
	});

	it('treats a stall timeout as benign while the document is hidden', async () => {
		vi.useFakeTimers();
		const doc = { hidden: true };
		vi.stubGlobal('document', doc);
		try {
			const source = new NativeAudioSource('local', { url: '/bg-stall.mp3', seekable: true });
			const onError = vi.fn();
			const states: string[] = [];
			source.onError(onError);
			source.onStateChange((s) => states.push(s));

			const loadPromise = source.load();
			hoisted.dispatch('canplay');
			await loadPromise;

			hoisted.dispatch('stalled');
			vi.advanceTimersByTime(15_000);
			expect(onError).not.toHaveBeenCalled();
			expect(states).not.toContain('error');

			// Still hidden — the re-armed timer stays benign indefinitely.
			vi.advanceTimersByTime(15_000);
			expect(onError).not.toHaveBeenCalled();

			// Once visible again, a real stall becomes fatal.
			doc.hidden = false;
			vi.advanceTimersByTime(15_000);
			expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'NETWORK_STALL' }));
		} finally {
			vi.unstubAllGlobals();
		}
	});

	it('throws when audio element is unavailable', () => {
		hoisted.getAudioElement.mockImplementationOnce(() => {
			throw new Error('Audio element not mounted');
		});

		expect(() => new NativeAudioSource('local', { url: '/missing.mp3', seekable: true })).toThrow(
			'Audio element not mounted'
		);
	});

	it('fires onProgress callback on timeupdate events', async () => {
		const source = new NativeAudioSource('local', { url: '/progress.mp3', seekable: true });
		const onProgress = vi.fn();
		source.onProgress(onProgress);

		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		hoisted.audio.currentTime = 42;
		hoisted.audio.duration = 180;
		hoisted.dispatch('timeupdate');

		expect(onProgress).toHaveBeenCalledWith(42, 180);
	});

	it('rejects load promise on media error event', async () => {
		const source = new NativeAudioSource('local', { url: '/bad.mp3', seekable: true });
		const onError = vi.fn();
		source.onError(onError);

		const loadPromise = source.load();

		hoisted.audio.error = { code: 4 } as MediaError;
		hoisted.dispatch('error');

		await expect(loadPromise).rejects.toThrow('MEDIA_ERR_SRC_NOT_SUPPORTED');
		expect(onError).toHaveBeenCalledWith(expect.objectContaining({ code: 'LOAD_ERROR' }));
	});

	it('transitions from buffering back to playing after seek via playing event', async () => {
		const source = new NativeAudioSource('local', { url: '/seek.mp3', seekable: true });
		const states: string[] = [];
		source.onStateChange((s) => states.push(s));

		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		hoisted.dispatch('play');
		expect(states).toContain('playing');

		hoisted.dispatch('waiting');
		expect(states.at(-1)).toBe('buffering');

		hoisted.dispatch('playing');
		expect(states.at(-1)).toBe('playing');
	});

	it('transitions from buffering back to playing via timeupdate fallback', async () => {
		const source = new NativeAudioSource('local', { url: '/seek2.mp3', seekable: true });
		const states: string[] = [];
		source.onStateChange((s) => states.push(s));

		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		hoisted.dispatch('play');
		hoisted.dispatch('waiting');
		expect(states.at(-1)).toBe('buffering');

		hoisted.audio.currentTime = 30;
		hoisted.dispatch('timeupdate');
		expect(states.at(-1)).toBe('playing');
	});

	it('does not emit redundant playing state on timeupdate when already playing', async () => {
		const source = new NativeAudioSource('local', { url: '/no-dup.mp3', seekable: true });
		const states: string[] = [];
		source.onStateChange((s) => states.push(s));

		const loadPromise = source.load();
		hoisted.dispatch('canplay');
		await loadPromise;

		hoisted.dispatch('play');
		const countAfterPlay = states.filter((s) => s === 'playing').length;

		hoisted.audio.currentTime = 10;
		hoisted.dispatch('timeupdate');

		expect(states.filter((s) => s === 'playing').length).toBe(countAfterPlay);
	});
});
