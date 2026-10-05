import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { apiGet, focus, playbackToast } = vi.hoisted(() => ({
	apiGet: vi.fn(),
	focus: { claim: vi.fn(), release: vi.fn(), interrupt: vi.fn() },
	playbackToast: { show: vi.fn(), dismiss: vi.fn() }
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { get: (...args: unknown[]) => apiGet(...args) } }
}));
vi.mock('$lib/stores/audioFocus.svelte', () => ({ audioFocus: focus }));
vi.mock('$lib/stores/playbackToast.svelte', () => ({ playbackToast }));

// A controllable stand-in for HTMLAudioElement: tests drive currentTime / ended
// to simulate a clip reaching its end, then let the ticker advance.
type DeferredPlay = {
	promise: Promise<void>;
	resolve: () => void;
	reject: (reason?: unknown) => void;
};

class FakeAudio {
	static created: FakeAudio[] = [];
	/** elements that attempted a real clip (excludes muted gesture unlocks) */
	static started: FakeAudio[] = [];
	static realPlayFailures: unknown[] = [];
	static deferredRealPlays: DeferredPlay[] = [];
	src = '';
	preload = '';
	muted = false;
	volume = 1;
	currentTime = 0;
	duration = 30;
	ended = false;
	play = vi.fn(() => {
		if (this.muted || !this.src) return Promise.resolve();
		FakeAudio.started.push(this);
		const deferred = FakeAudio.deferredRealPlays.shift();
		if (deferred) return deferred.promise;
		const failure = FakeAudio.realPlayFailures.shift();
		return failure === undefined ? Promise.resolve() : Promise.reject(failure);
	});
	pause = vi.fn(() => {});
	removeAttribute = vi.fn((name: string) => {
		if (name === 'src') this.src = '';
	});
	load = vi.fn(() => {
		this.currentTime = 0;
		this.ended = false;
	});
	constructor() {
		FakeAudio.created.push(this);
	}
	/** create a play promise that a test can settle after a restart */
	static deferRealPlay(): DeferredPlay {
		let resolve!: () => void;
		let reject!: (reason?: unknown) => void;
		const promise = new Promise<void>((res, rej) => {
			resolve = res;
			reject = rej;
		});
		const deferred = { promise, resolve, reject };
		FakeAudio.deferredRealPlays.push(deferred);
		return deferred;
	}
	/** simulate this clip finishing */
	finish() {
		this.currentTime = this.duration;
		this.ended = true;
	}
}

import { deckSampler } from './deckSampler.svelte';

function albumPreview(n: number, provider = 'deezer') {
	return {
		provider,
		tracks: Array.from({ length: n }, (_, i) => ({
			title: `Track ${i + 1}`,
			artist_name: 'Artist',
			preview_url: `https://p/${i}.mp3`,
			duration_s: 30,
			position: i + 1
		}))
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	FakeAudio.created = [];
	FakeAudio.started = [];
	FakeAudio.realPlayFailures = [];
	FakeAudio.deferredRealPlays = [];
	(globalThis as unknown as { Audio: typeof FakeAudio }).Audio = FakeAudio;
});

afterEach(() => {
	deckSampler.stop();
	playbackToast.dismiss();
	vi.useRealTimers();
});

async function waitForStatus(status: string) {
	await vi.waitFor(() => expect(deckSampler.status).toBe(status));
}

describe('deckSampler station', () => {
	it('rapid next() never starts two <audio> at once (session-guarded race)', async () => {
		// deferred fetches so we can pile up skips before any entry resolves
		const resolvers: ((v: unknown) => void)[] = [];
		apiGet.mockImplementation(
			() => new Promise((resolve) => resolvers.push(resolve as (v: unknown) => void))
		);

		deckSampler.startStation('Station', [
			{ key: 'rg-1', kind: 'album', artist: 'A1', title: 'Album 1', albumMbid: 'rg-1' },
			{ key: 'rg-2', kind: 'album', artist: 'A2', title: 'Album 2', albumMbid: 'rg-2' },
			{ key: 'rg-3', kind: 'album', artist: 'A3', title: 'Album 3', albumMbid: 'rg-3' }
		]);
		// entry 0 is fetching; skip twice before it (or entry 1) resolves
		deckSampler.next();
		deckSampler.next();

		// now let all three in-flight fetches resolve; only the last (current session)
		// chain should survive its `mySession !== session` guard and play
		resolvers.forEach((r) => r(albumPreview(2)));
		await vi.waitFor(() => expect(deckSampler.status).toBe('playing'));

		expect(deckSampler.stationPosition.index).toBe(2);
		expect(deckSampler.currentEntry?.title).toBe('Album 3');
		// exactly one element was ever started -> no double audio
		expect(FakeAudio.started.length).toBe(1);
	});

	it('ignores a stale play fulfillment after a restart', async () => {
		vi.useFakeTimers();
		apiGet.mockResolvedValue(albumPreview(1));
		const stalePlay = FakeAudio.deferRealPlay();

		deckSampler.start('rg-old', 'Artist', 'Old');
		await vi.waitFor(() => expect(FakeAudio.started).toHaveLength(1));

		deckSampler.start('rg-new', 'Artist', 'New');
		await waitForStatus('playing');
		await vi.waitFor(() => expect(FakeAudio.started).toHaveLength(2));
		const current = FakeAudio.started.at(-1)!;

		current.currentTime = 15;
		stalePlay.resolve();
		await vi.advanceTimersByTimeAsync(100);
		current.currentTime = 20;
		await vi.advanceTimersByTimeAsync(100);

		expect(deckSampler.activeKey).toBe('rg-new');
		expect(deckSampler.currentTrack?.title).toBe('Track 1');
		expect(deckSampler.progress).toBeCloseTo(20 / current.duration);
	});
	it('does not let a stale fade timer pause a reused element after restart', async () => {
		vi.useFakeTimers();
		apiGet.mockResolvedValue(albumPreview(2));

		deckSampler.start('rg-old', 'Artist', 'Old');
		await waitForStatus('playing');
		const first = FakeAudio.started.at(-1)!;
		first.currentTime = first.duration - 0.25;
		await vi.advanceTimersByTimeAsync(100);
		await vi.waitFor(() => expect(FakeAudio.started).toHaveLength(2));

		deckSampler.start('rg-new', 'Artist', 'New');
		await waitForStatus('playing');
		await vi.waitFor(() => expect(FakeAudio.started).toHaveLength(3));
		const replacement = FakeAudio.started.at(-1)!;
		replacement.pause.mockClear();

		await vi.advanceTimersByTimeAsync(600);

		expect(deckSampler.status).toBe('playing');
		expect(replacement.pause).not.toHaveBeenCalled();
	});
});

describe('deckSampler transport', () => {
	it('pauses with feedback when autoplay blocks a clip and resumes from a gesture', async () => {
		vi.useFakeTimers();
		apiGet.mockResolvedValue(albumPreview(2));
		FakeAudio.realPlayFailures.push({ name: 'NotAllowedError' });

		deckSampler.start('rg-blocked', 'Artist', 'Blocked');
		await waitForStatus('paused');

		expect(playbackToast.show).toHaveBeenCalledWith(
			expect.stringContaining('Tap the preview play button'),
			'warning'
		);
		const el = FakeAudio.started.at(-1)!;
		el.play.mockClear();

		deckSampler.resume();
		await waitForStatus('playing');
		expect(el.play).toHaveBeenCalledTimes(1);
	});
});
