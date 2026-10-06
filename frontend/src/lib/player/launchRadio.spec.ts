import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { RadioPlanTrack } from '$lib/types';

const { apiMock, playerMock, toastMock } = vi.hoisted(() => ({
	apiMock: { global: { get: vi.fn(), post: vi.fn() } },
	playerMock: {
		queue: [] as unknown[],
		currentIndex: 0,
		isPlaying: false,
		playQueue: vi.fn(),
		addMultipleToQueue: vi.fn(),
		patchQueueItemByPlaylistTrackId: vi.fn(),
		removeFromQueue: vi.fn(),
		pause: vi.fn()
	},
	toastMock: { show: vi.fn() }
}));

vi.mock('$lib/api/client', () => ({
	api: { global: { v3: { GET: apiMock.global.get, POST: apiMock.global.post } } }
}));
vi.mock('$lib/stores/player.svelte', () => ({ playerStore: playerMock }));
vi.mock('$lib/stores/playbackToast.svelte', () => ({ playbackToast: toastMock }));
const resolveRadioPatch = vi.hoisted(() => vi.fn());
vi.mock('$lib/player/radioQueueHydrator', () => ({
	startRadioHydration: vi.fn(),
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

import { launchRadio, planTrackToQueueItem } from './launchRadio';
import { radioSession } from '$lib/stores/radioSession.svelte';

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
	});

	it('resolves the first YouTube placeholder BEFORE playback starts', async () => {
		apiMock.global.post.mockResolvedValue({ title: 'Radio', tracks: [track()] });
		resolveRadioPatch.mockResolvedValue({ trackSourceId: 'vid-1' });

		const ok = await launchRadio({ seed_type: 'genre', seed_id: 'x' }, true);

		expect(ok).toBe(true);
		expect(resolveRadioPatch).toHaveBeenCalledTimes(1);
		const [items] = playerMock.playQueue.mock.calls[0];
		expect(items[0].trackSourceId).toBe('vid-1');
	});

	it('drops dead heads and aborts when nothing resolves', async () => {
		apiMock.global.post.mockResolvedValue({ title: 'Radio', tracks: [track()] });
		resolveRadioPatch.mockResolvedValue(null);

		const ok = await launchRadio({ seed_type: 'genre', seed_id: 'x' }, true);

		expect(ok).toBe(false);
		expect(playerMock.playQueue).not.toHaveBeenCalled();
		expect(radioSession.active).toBe(false);
	});

	it('dedupes tracks within the complete plan', async () => {
		apiMock.global.post.mockResolvedValue({
			title: 'Radio',
			tracks: [track(), track()] // duplicate in one plan
		});

		await launchRadio({ seed_type: 'artist', seed_id: 'a-1' }, true);

		const [items] = playerMock.playQueue.mock.calls[0];
		expect(items).toHaveLength(1);
	});

	it('does not start playback when the player closes during tuning', async () => {
		let finishPlan: (value: { title: string; tracks: RadioPlanTrack[] }) => void = () => {};
		apiMock.global.post.mockImplementationOnce(
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
		apiMock.global.post
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
});
