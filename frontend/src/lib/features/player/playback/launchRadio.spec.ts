import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { RadioPlanTrack } from '$lib/features/discover/queries/types';

const { apiMock, playerMock, toastMock } = vi.hoisted(() => ({
	apiMock: { get: vi.fn(), post: vi.fn() },
	playerMock: {
		queue: [] as unknown[],
		currentIndex: 0,
		currentQueueItem: null as unknown,
		shuffleEnabled: false,
		shuffleOrder: [] as number[],
		upcomingQueueLength: 0,
		isPlaying: false,
		playQueue: vi.fn(),
		addMultipleToQueue: vi.fn(),
		appendQueueSilent: vi.fn(),
		nextTrack: vi.fn(),
		patchQueueItemByPlaylistTrackId: vi.fn(),
		removeFromQueue: vi.fn(),
		pause: vi.fn()
	},
	toastMock: { notify: vi.fn() }
}));

vi.mock('$lib/api/client', () => ({
	api: { v3: { GET: apiMock.get, POST: apiMock.post } }
}));
vi.mock('$lib/features/player/state/player.svelte', () => ({ playerStore: playerMock }));
vi.mock('$lib/stores/toast', () => ({ toastStore: toastMock }));
const resolveRadioPatch = vi.hoisted(() => vi.fn());
const startRadioHydration = vi.hoisted(() => vi.fn());
vi.mock('$lib/features/player/playback/radioQueueHydrator', () => ({
	startRadioHydration: (...args: unknown[]) => startRadioHydration(...args),
	stopRadioHydration: vi.fn(),
	prepareRadioHydration: vi.fn(),
	resolveRadioPatch: (...args: unknown[]) => resolveRadioPatch(...args),
	needsHydration: (item: {
		sourceType: string;
		trackSourceId?: string;
		playlistTrackId?: string;
	}) => {
		if (!item.playlistTrackId?.startsWith('radio:')) return false;
		return item.sourceType === 'youtube' && !item.trackSourceId;
	}
}));

import {
	ASK_WHEN_LEFT,
	NEXT_CHUNK,
	extendRadio,
	launchRadio,
	notePlaying,
	planTrackToQueueItem
} from './launchRadio';
import { radioSession } from '$lib/features/player/state/radioSession.svelte';

function track(overrides: Partial<RadioPlanTrack> = {}): RadioPlanTrack {
	return {
		track_name: 'Song',
		artist_name: 'Artist',
		artist_mbid: 'a-1',
		recording_mbid: 'rec-1',
		album_mbid: 'rg-1',
		album_name: 'Album',
		in_library: false,
		local_file_id: null,
		file_format: null,
		duration_s: null,
		...overrides
	};
}

describe('planTrackToQueueItem tiers', () => {
	it('un-owned tracks without YouTube are dropped (previews are not a player tier)', () => {
		const item = planTrackToQueueItem(track(), false);
		expect(item).toBeNull();
	});
});

describe('launchRadio', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		resolveRadioPatch.mockResolvedValue({ trackSourceId: 'vid-default' });
		radioSession.end();
		playerMock.queue = [];
		playerMock.currentIndex = 0;
		playerMock.currentQueueItem = null;
		playerMock.upcomingQueueLength = 0;
	});

	it('resolves the first YouTube placeholder BEFORE playback starts', async () => {
		apiMock.post.mockResolvedValue({ title: 'Radio', tracks: [track()] });
		resolveRadioPatch.mockResolvedValue({ trackSourceId: 'vid-1' });

		const ok = await launchRadio({ seed_type: 'genre', seed_id: 'x' }, true);

		expect(ok).toBe(true);
		expect(resolveRadioPatch).toHaveBeenCalledTimes(1);
		const [items] = playerMock.playQueue.mock.calls[0];
		expect(items[0].trackSourceId).toBe('vid-1');
	});

	it('drops dead heads and aborts when nothing resolves', async () => {
		apiMock.post.mockResolvedValue({ title: 'Radio', tracks: [track()] });
		resolveRadioPatch.mockResolvedValue(null);

		const ok = await launchRadio({ seed_type: 'genre', seed_id: 'x' }, true);

		expect(ok).toBe(false);
		expect(playerMock.playQueue).not.toHaveBeenCalled();
		expect(radioSession.active).toBe(false);
	});

	it('dedupes tracks within the complete plan', async () => {
		apiMock.post.mockResolvedValue({
			title: 'Radio',
			tracks: [track(), track()] // duplicate in one plan
		});

		await launchRadio({ seed_type: 'artist', seed_id: 'a-1' }, true);

		const [items] = playerMock.playQueue.mock.calls[0];
		expect(items).toHaveLength(1);
	});

	it('does not start playback when the player closes during tuning', async () => {
		let finishPlan: (value: { title: string; tracks: RadioPlanTrack[] }) => void = () => {};
		apiMock.post.mockImplementationOnce(
			() =>
				new Promise((resolve) => {
					finishPlan = resolve;
				})
		);

		const tuning = launchRadio({ seed_type: 'artist', seed_id: 'slow' }, true);
		radioSession.end();
		finishPlan({ title: 'Late station', tracks: [track()] });

		expect(await tuning).toBe(false);
		expect(playerMock.playQueue).not.toHaveBeenCalled();
	});

	it('lets the newest station win when plans finish out of order', async () => {
		let finishFirst: (value: { title: string; tracks: RadioPlanTrack[] }) => void = () => {};
		let finishSecond: (value: { title: string; tracks: RadioPlanTrack[] }) => void = () => {};
		apiMock.post
			.mockImplementationOnce(
				() =>
					new Promise((resolve) => {
						finishFirst = resolve;
					})
			)
			.mockImplementationOnce(
				() =>
					new Promise((resolve) => {
						finishSecond = resolve;
					})
			);

		const first = launchRadio({ seed_type: 'artist', seed_id: 'first' }, true);
		const second = launchRadio({ seed_type: 'artist', seed_id: 'second' }, true);
		finishSecond({ title: 'Second', tracks: [track({ track_name: 'Second' })] });
		expect(await second).toBe(true);
		finishFirst({ title: 'First', tracks: [track({ track_name: 'First' })] });

		expect(await first).toBe(false);
		expect(playerMock.playQueue).toHaveBeenCalledOnce();
		expect(playerMock.playQueue.mock.calls[0][0][0].trackName).toBe('Second');
	});

	describe('endless', () => {
		const owned = (n: number) =>
			track({
				track_name: `Song ${n}`,
				in_library: true,
				local_file_id: `t${n}`,
				recording_mbid: null
			});
		const ids = (from: number, to: number) =>
			Array.from({ length: to - from + 1 }, (_, n) => `t${from + n}`);

		async function playing(tracks: number): Promise<void> {
			playerMock.playQueue.mockImplementation((items: unknown[]) => {
				playerMock.queue = items;
				playerMock.currentIndex = 0;
				playerMock.currentQueueItem = items[0];
			});
			apiMock.post.mockResolvedValueOnce({
				title: 'Radio',
				tracks: Array.from({ length: tracks }, (_, n) => owned(n + 1))
			});
			expect(
				await launchRadio({ seed_type: 'track', seed_id: 't1' }, false, { mode: 'library' })
			).toBe(true);
		}

		/** Play on to queue index `at`, as the player does: it keeps 3 played tracks. */
		function playTo(at: number): void {
			const all = playerMock.queue as { trackSourceId: string }[];
			for (let index = playerMock.currentIndex + 1; index <= at; index++) {
				playerMock.currentIndex = index;
				playerMock.currentQueueItem = all[index];
				notePlaying();
			}
			const dropped = Math.max(0, at - 3);
			playerMock.queue = all.slice(dropped);
			playerMock.currentIndex = at - dropped;
			playerMock.upcomingQueueLength = playerMock.queue.length - playerMock.currentIndex - 1;
		}

		it('asks for the next chunk at 5 left, sending every track the station played in order', async () => {
			await playing(10);
			playTo(3);
			expect(await extendRadio()).toBe(false);
			expect(apiMock.post).toHaveBeenCalledTimes(1);

			playTo(6);
			expect(playerMock.queue).toHaveLength(7);
			apiMock.post.mockResolvedValueOnce({ title: 'Radio', tracks: [owned(11), owned(12)] });
			expect(await extendRadio()).toBe(true);
			const [, body] = apiMock.post.mock.calls[1];
			expect(body).toMatchObject({
				seed_type: 'track',
				seed_id: 't1',
				mode: 'library',
				count: NEXT_CHUNK,
				more_like: [],
				not_this: []
			});
			expect(
				body.station_tracks.map((sent: { local_track_id: string }) => sent.local_track_id)
			).toEqual(ids(1, 10));
			expect(body.station_tracks[0]).toMatchObject({ artist_name: 'Artist', track_name: 'Song 1' });
			const [appended] = playerMock.appendQueueSilent.mock.calls[0];
			expect(appended.map((item: { trackSourceId: string }) => item.trackSourceId)).toEqual([
				't11',
				't12'
			]);
		});

		it('asks for more from the hydrator tick the station starts', async () => {
			await playing(6);
			const [tick] = startRadioHydration.mock.calls[0];
			playerMock.upcomingQueueLength = ASK_WHEN_LEFT;
			apiMock.post.mockResolvedValueOnce({ title: 'Radio', tracks: [owned(7)] });
			tick();
			await vi.waitFor(() => expect(playerMock.appendQueueSilent).toHaveBeenCalledOnce());
			expect(apiMock.post).toHaveBeenCalledTimes(2);
		});

		it('tries a failed ask again after a wait, and does not end the station', async () => {
			const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
			const now = vi.spyOn(Date, 'now').mockReturnValue(1_000_000);
			try {
				await playing(6);
				playerMock.upcomingQueueLength = ASK_WHEN_LEFT;
				apiMock.post.mockRejectedValueOnce(new Error('offline'));
				expect(await extendRadio()).toBe(false);
				expect(radioSession.exhausted).toBe(false);
				expect(await extendRadio()).toBe(false);
				expect(apiMock.post).toHaveBeenCalledTimes(2);

				now.mockReturnValue(1_000_000 + 60_000);
				apiMock.post.mockResolvedValueOnce({ title: 'Radio', tracks: [owned(7)] });
				expect(await extendRadio()).toBe(true);
				expect(apiMock.post).toHaveBeenCalledTimes(3);
				expect(warn).toHaveBeenCalledOnce();
			} finally {
				now.mockRestore();
				warn.mockRestore();
			}
		});

		it('drops an answer that comes after the player stopped, so nothing plays again', async () => {
			await playing(6);
			playerMock.upcomingQueueLength = 1;
			let answer: (value: { title: string; tracks: RadioPlanTrack[] }) => void = () => {};
			apiMock.post.mockImplementationOnce(
				() =>
					new Promise((resolve) => {
						answer = resolve;
					})
			);
			const asking = extendRadio();
			// Stop clears the queue and ends the station, as the player does.
			playerMock.queue = [];
			radioSession.end();
			answer({ title: 'Radio', tracks: [owned(7)] });

			expect(await asking).toBe(false);
			expect(playerMock.appendQueueSilent).not.toHaveBeenCalled();
			expect(playerMock.playQueue).toHaveBeenCalledOnce();
		});

		it('carries each track reason onto its queue item', async () => {
			const reason = {
				kind: 'sounds_like' as const,
				line: 'Sounds close to Supernatural',
				seed_name: 'Supernatural',
				seed_track_id: 't1',
				closeness: 'close' as const
			};
			playerMock.playQueue.mockImplementation((items: unknown[]) => {
				playerMock.queue = items;
			});
			apiMock.post.mockResolvedValueOnce({ title: 'Radio', tracks: [{ ...owned(2), reason }] });
			await launchRadio({ seed_type: 'track', seed_id: 't1' }, false);
			const [items] = playerMock.playQueue.mock.calls[0];
			expect(items[0].reason).toEqual(reason);
		});

		it('says once that a station plays the library only when nothing plays the rest', async () => {
			await playing(6);
			expect(toastMock.notify).not.toHaveBeenCalled();
			radioSession.end();

			playerMock.playQueue.mockClear();
			apiMock.post.mockResolvedValueOnce({
				title: 'Radio',
				tracks: [owned(1), owned(2)],
				library_only: true
			});
			await launchRadio({ seed_type: 'track', seed_id: 't1' }, false);
			expect(toastMock.notify).toHaveBeenCalledTimes(1);
			expect(toastMock.notify.mock.calls[0][0]).toMatch(/your library/i);

			playerMock.upcomingQueueLength = 1;
			apiMock.post.mockResolvedValueOnce({
				title: 'Radio',
				tracks: [owned(3)],
				library_only: true
			});
			await extendRadio();
			expect(toastMock.notify).toHaveBeenCalledTimes(1);
		});
	});
});
