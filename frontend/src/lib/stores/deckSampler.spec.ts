import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { apiGet, focus } = vi.hoisted(() => ({
	apiGet: vi.fn(),
	focus: { claim: vi.fn(), release: vi.fn(), interrupt: vi.fn() }
}));
vi.mock('$lib/api/client', () => ({
	api: { global: { get: (...args: unknown[]) => apiGet(...args) } }
}));
vi.mock('$lib/stores/audioFocus.svelte', () => ({ audioFocus: focus }));

// A controllable stand-in for HTMLAudioElement: tests drive currentTime / ended
// to simulate a clip reaching its end, then let the ticker advance.
class FakeAudio {
	static created: FakeAudio[] = [];
	/** elements that started a real clip (excludes the gesture-unlock blip);
	 * the sampler reuses a persistent pool, so track starts, not constructions */
	static started: FakeAudio[] = [];
	src = '';
	preload = '';
	muted = false;
	volume = 1;
	currentTime = 0;
	duration = 30;
	ended = false;
	play = vi.fn(async () => {
		if (!this.src.startsWith('data:')) FakeAudio.started.push(this);
	});
	pause = vi.fn(() => {});
	constructor() {
		FakeAudio.created.push(this);
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
	(globalThis as unknown as { Audio: typeof FakeAudio }).Audio = FakeAudio;
});

afterEach(() => {
	deckSampler.stop();
});

async function waitForStatus(status: string) {
	await vi.waitFor(() => expect(deckSampler.status).toBe(status));
}

describe('deckSampler single album', () => {
	it('plays an album’s clips back-to-back then ends', async () => {
		apiGet.mockResolvedValue(albumPreview(2));

		deckSampler.start('rg-1', 'Artist', 'Album', { artistMbid: 'a-1', coverUrl: 'c.jpg' });
		await waitForStatus('playing');

		expect(deckSampler.currentEntry?.title).toBe('Album');
		expect(deckSampler.currentEntry?.albumMbid).toBe('rg-1');
		expect(deckSampler.provider).toBe('deezer');
		expect(deckSampler.trackIndex).toBe(0);

		// clip 1 ends -> advance to clip 2
		FakeAudio.started.at(-1)!.finish();
		await vi.waitFor(() => expect(deckSampler.trackIndex).toBe(1));

		// clip 2 ends -> station of one is exhausted -> idle
		FakeAudio.started.at(-1)!.finish();
		await waitForStatus('idle');
		expect(focus.release).toHaveBeenCalled();
	});

	it('reports error when the album has no previews', async () => {
		apiGet.mockResolvedValue(albumPreview(0));
		deckSampler.start('rg-x', 'Artist', 'Nope');
		await waitForStatus('error');
	});
});

describe('deckSampler station', () => {
	it('advances from one album to the next', async () => {
		apiGet.mockResolvedValue(albumPreview(2));

		deckSampler.startStation('Station', [
			{ key: 'rg-1', kind: 'album', artist: 'A1', title: 'Album 1', albumMbid: 'rg-1' },
			{ key: 'rg-2', kind: 'album', artist: 'A2', title: 'Album 2', albumMbid: 'rg-2' }
		]);
		await waitForStatus('playing');
		expect(deckSampler.isStation).toBe(true);
		expect(deckSampler.stationPosition).toEqual({ index: 0, total: 2 });

		// exhaust the first album's 2 clips -> should load the second entry
		FakeAudio.started.at(-1)!.finish();
		await vi.waitFor(() => expect(deckSampler.trackIndex).toBe(1));
		FakeAudio.started.at(-1)!.finish();
		await vi.waitFor(() => expect(deckSampler.stationPosition.index).toBe(1));
		expect(deckSampler.currentEntry?.title).toBe('Album 2');
	});

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
		// exactly one clip was ever started -> no double audio
		expect(FakeAudio.started.length).toBe(1);
	});

	it('next() skips to the following entry immediately', async () => {
		apiGet.mockResolvedValue(albumPreview(2));
		deckSampler.startStation('Station', [
			{ key: 'rg-1', kind: 'album', artist: 'A1', title: 'Album 1', albumMbid: 'rg-1' },
			{ key: 'rg-2', kind: 'album', artist: 'A2', title: 'Album 2', albumMbid: 'rg-2' }
		]);
		await waitForStatus('playing');
		expect(deckSampler.hasNext).toBe(true);

		deckSampler.next();
		await vi.waitFor(() => expect(deckSampler.stationPosition.index).toBe(1));
		expect(deckSampler.hasNext).toBe(false);
	});
});

describe('deckSampler transport', () => {
	it('pause halts the ticker and audio; resume continues', async () => {
		apiGet.mockResolvedValue(albumPreview(2));
		deckSampler.start('rg-1', 'Artist', 'Album');
		await waitForStatus('playing');
		const el = FakeAudio.started.at(-1)!;

		deckSampler.pause();
		expect(deckSampler.status).toBe('paused');
		expect(el.pause).toHaveBeenCalled();

		el.play.mockClear();
		deckSampler.resume();
		expect(deckSampler.status).toBe('playing');
		expect(el.play).toHaveBeenCalled();
	});

	it('setVolume applies live to the active element and persists', async () => {
		apiGet.mockResolvedValue(albumPreview(1));
		deckSampler.start('rg-1', 'Artist', 'Album');
		await waitForStatus('playing');
		const el = FakeAudio.started.at(-1)!;

		deckSampler.setVolume(0.4);
		expect(deckSampler.volume).toBe(0.4);
		expect(el.volume).toBe(0.4);
	});

	it('stop clears the station and releases focus', async () => {
		apiGet.mockResolvedValue(albumPreview(2));
		deckSampler.start('rg-1', 'Artist', 'Album');
		await waitForStatus('playing');

		deckSampler.stop();
		expect(deckSampler.status).toBe('idle');
		expect(deckSampler.currentEntry).toBeNull();
		expect(deckSampler.activeKey).toBe('');
		expect(focus.release).toHaveBeenCalled();
	});
});
