import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { apiGet, focus, toastStore } = vi.hoisted(() => ({
	apiGet: vi.fn(),
	focus: { claim: vi.fn(), release: vi.fn(), interrupt: vi.fn() },
	toastStore: { notify: vi.fn(), hide: vi.fn() }
}));
// Every preview read goes straight to the scripted answer, never a cache.
vi.mock('$lib/features/discover/queries/previews', () => ({
	fetchTrackPreview: (...args: unknown[]) => apiGet(...args),
	fetchAlbumPreview: (...args: unknown[]) => apiGet(...args)
}));
vi.mock('$lib/features/player/state/audioFocus.svelte', () => ({ audioFocus: focus }));
vi.mock('$lib/stores/toast', () => ({ toastStore }));

// A controllable stand-in for HTMLAudioElement: tests drive currentTime / ended
// to simulate a clip reaching its end, then let the ticker advance.
type DeferredPlay = {
	promise: Promise<void>;
	resolve: () => void;
	reject: (reason?: unknown) => void;
};

class FakeAudio extends EventTarget {
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
		super();
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
		this.dispatchEvent(new Event('ended'));
	}
}

import { deckSampler, type SampleEntry } from './deckSampler.svelte';
import { pickAnswers } from './pickAnswers.svelte';

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
	toastStore.hide();
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

		expect(toastStore.notify).toHaveBeenCalledWith(
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

const ROLES = ['opener', 'best_known', 'deep_cut'] as const;

/** An audition answer: up to three clips named `<album> <role>`. */
function audition(album: string, count = 3) {
	return {
		provider: 'deezer',
		tracks: ROLES.slice(0, count).map((role, i) => ({
			title: `${album} ${role}`,
			artist_name: 'Artist',
			preview_url: `https://p/${album.replace(/ /g, '-')}/${i}.mp3`,
			duration_s: 30,
			position: i + 1,
			role,
			source: 'deezer'
		}))
	};
}

/** A week pick as the counter hands it over. */
function pick(n: number, onFirstClip?: () => void): SampleEntry {
	return {
		key: `rg-${n}`,
		kind: 'album',
		artist: 'Artist',
		title: `Album ${n}`,
		albumMbid: `rg-${n}`,
		mode: 'audition',
		onFirstClip
	};
}

/** What is sounding now, as "<album> <role>". */
const sounding = () => deckSampler.currentTrack?.title;

/** Lets the playing clip run out and waits for the next to start, or for the end. */
async function finishClip() {
	const before = FakeAudio.started.length;
	FakeAudio.started.at(-1)!.finish();
	await vi.advanceTimersByTimeAsync(100);
	await vi.waitFor(() =>
		expect(FakeAudio.started.length > before || deckSampler.status === 'idle').toBe(true)
	);
}

/** The albums the preview reads were asked for, in order, with their mode. */
const asked = () => apiGet.mock.calls.map(([, album, mode]) => `${album} ${mode}`);

describe('deckSampler: Play the week', () => {
	it('plays three clips an album in order, then the next album, and ends after the last', async () => {
		vi.useFakeTimers();
		apiGet.mockImplementation((_artist: string, album: string) => Promise.resolve(audition(album)));

		deckSampler.startStation('Week 47', [pick(1), pick(2)], 'week');
		await waitForStatus('playing');
		const heard: (string | undefined)[] = [];
		for (let clip = 0; clip < 6; clip++) {
			heard.push(sounding());
			await finishClip();
		}

		expect(heard).toEqual([
			'Album 1 opener',
			'Album 1 best_known',
			'Album 1 deep_cut',
			'Album 2 opener',
			'Album 2 best_known',
			'Album 2 deep_cut'
		]);
		expect(deckSampler.status).toBe('idle');
		expect(toastStore.notify).not.toHaveBeenCalled();
	});

	it('passes over a pick with no preview without a toast, and goes on', async () => {
		vi.useFakeTimers();
		apiGet.mockImplementation((_artist: string, album: string) =>
			Promise.resolve(album === 'Album 1' ? audition(album) : audition(album, 0))
		);

		deckSampler.startStation('Week 47', [pick(1), pick(2), pick(3)], 'week');
		await waitForStatus('playing');
		for (let clip = 0; clip < 3; clip++) await finishClip();

		expect(deckSampler.status).toBe('idle');
		expect(toastStore.notify).not.toHaveBeenCalled();

		apiGet.mockImplementation((_artist: string, album: string) =>
			Promise.resolve(album === 'Album 2' ? audition(album, 0) : audition(album, 1))
		);
		deckSampler.startStation('Week 47', [pick(1), pick(2), pick(3)], 'week');
		await waitForStatus('playing');
		await finishClip();
		expect(sounding()).toBe('Album 3 opener');
		expect(toastStore.notify).not.toHaveBeenCalled();
	});

	it('fetches the next album while one plays, no further, and loads its first clip ahead', async () => {
		vi.useFakeTimers();
		apiGet.mockImplementation((_artist: string, album: string) => Promise.resolve(audition(album)));

		deckSampler.startStation('Week 47', [pick(1), pick(2), pick(3)], 'week');
		await waitForStatus('playing');
		await vi.waitFor(() => expect(asked()).toEqual(['Album 1 audition', 'Album 2 audition']));

		await finishClip();
		await finishClip();
		await vi.advanceTimersByTimeAsync(100);
		const waiting = FakeAudio.started.find((el) => el.src === 'https://p/Album-2/0.mp3');
		expect(waiting, 'the next album first clip waits in the idle element').toBeDefined();
		expect(asked()).toEqual(['Album 1 audition', 'Album 2 audition']);

		await finishClip();
		expect(sounding()).toBe('Album 2 opener');
		expect(FakeAudio.started.at(-1)).toBe(waiting);
		await vi.waitFor(() =>
			expect(asked()).toEqual(['Album 1 audition', 'Album 2 audition', 'Album 3 audition'])
		);
	});

	it('reports each pick once, at its first clip, and sends nothing else anywhere', async () => {
		vi.useFakeTimers();
		const fetchSpy = vi.spyOn(globalThis, 'fetch');
		apiGet.mockImplementation((_artist: string, album: string) => Promise.resolve(audition(album)));
		const first = vi.fn();
		const second = vi.fn();

		deckSampler.startStation('Week 47', [pick(1, first), pick(2, second)], 'week');
		await waitForStatus('playing');
		expect(first).toHaveBeenCalledTimes(1);
		expect(second).not.toHaveBeenCalled();
		for (let clip = 0; clip < 4; clip++) await finishClip();

		expect(sounding()).toBe('Album 2 best_known');
		expect(first).toHaveBeenCalledTimes(1);
		expect(second).toHaveBeenCalledTimes(1);
		// a sample is no play: no scrobble, no playback event, no request at all
		expect(fetchSpy).not.toHaveBeenCalled();
		fetchSpy.mockRestore();
	});

	it('passes over a pick answered on the counter after the week started', async () => {
		vi.useFakeTimers();
		apiGet.mockImplementation((_artist: string, album: string) =>
			Promise.resolve(audition(album, 1))
		);
		const answered = new Set<string>();
		const clear = pickAnswers.register({ open: (key) => !answered.has(key) });
		const week = [1, 2, 3].map((n) => ({ ...pick(n), pick: 'weekly_set' as const }));

		deckSampler.startStation('Week 47', week, 'week');
		await waitForStatus('playing');
		answered.add('rg-2');
		await finishClip();

		expect(sounding()).toBe('Album 3 opener');
		clear();
	});

	it('stops when the main player starts', async () => {
		apiGet.mockImplementation((_artist: string, album: string) => Promise.resolve(audition(album)));

		deckSampler.startStation('Week 47', [pick(1), pick(2)], 'week');
		await waitForStatus('playing');
		const playing = FakeAudio.started.at(-1)!;
		// the player starting interrupts whoever holds audio focus
		const [id, yieldFocus] = focus.claim.mock.calls.at(-1)!;
		expect(id).toBe('deck-sampler');
		yieldFocus();

		expect(deckSampler.status).toBe('idle');
		expect(playing.pause).toHaveBeenCalled();
	});
});

describe('deckSampler: a pick sampled again', () => {
	it('moves on one clip each time its play is pressed again', async () => {
		apiGet.mockImplementation((_artist: string, album: string) => Promise.resolve(audition(album)));
		const heard: (string | undefined)[] = [];
		for (let press = 0; press < 4; press++) {
			deckSampler.start('rg-1', 'Artist', 'Album 1', { mode: 'audition' });
			await waitForStatus('playing');
			heard.push(sounding());
			deckSampler.stop();
		}

		expect(heard).toEqual([
			'Album 1 opener',
			'Album 1 best_known',
			'Album 1 deep_cut',
			'Album 1 opener'
		]);
	});
});
