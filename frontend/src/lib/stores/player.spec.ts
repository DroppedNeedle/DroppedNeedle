/* eslint-disable @typescript-eslint/no-unsafe-function-type */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { QueueItem } from '$lib/player/types';

type StateCallback = (state: import('$lib/player/types').PlaybackState) => void;
type ProgressCallback = (currentTime: number, duration: number) => void;
type ErrorCallback = (error: { code: string; message: string }) => void;

let capturedStateCallbacks: StateCallback[] = [];
let capturedProgressCallbacks: ProgressCallback[] = [];
let capturedErrorCallbacks: ErrorCallback[] = [];

vi.mock('$lib/player/createSource', () => ({
	createPlaybackSource: vi.fn(() => {
		capturedStateCallbacks = [];
		capturedProgressCallbacks = [];
		capturedErrorCallbacks = [];
		return {
			type: 'local' as const,
			load: vi.fn().mockResolvedValue(undefined),
			play: vi.fn(),
			pause: vi.fn(),
			seekTo: vi.fn(),
			setVolume: vi.fn(),
			getCurrentTime: vi.fn(() => 0),
			getDuration: vi.fn(() => 180),
			isSeekable: vi.fn(() => true),
			destroy: vi.fn(),
			onStateChange: vi.fn((cb: StateCallback) => {
				capturedStateCallbacks.push(cb);
			}),
			onReady: vi.fn(),
			onError: vi.fn((cb: ErrorCallback) => {
				capturedErrorCallbacks.push(cb);
			}),
			onProgress: vi.fn((cb: ProgressCallback) => {
				capturedProgressCallbacks.push(cb);
			})
		};
	})
}));

vi.mock('$lib/player/jellyfinPlaybackApi', () => ({
	startSession: vi.fn(async () => 'mock-session'),
	reportProgress: vi.fn(async () => true),
	reportStop: vi.fn(async () => true)
}));

const storage = new Map<string, string>();
vi.stubGlobal('localStorage', {
	getItem: vi.fn((key: string) => (storage.has(key) ? storage.get(key)! : null)),
	setItem: vi.fn((key: string, value: string) => {
		storage.set(key, value);
	}),
	removeItem: vi.fn((key: string) => {
		storage.delete(key);
	}),
	clear: vi.fn(() => {
		storage.clear();
	})
});

vi.mock('$lib/stores/playbackToast.svelte', () => ({
	playbackToast: {
		show: vi.fn(),
		dismiss: vi.fn(),
		get visible() {
			return false;
		},
		get message() {
			return '';
		},
		get type() {
			return 'info' as const;
		}
	}
}));

const mockApiGet = vi.fn();
const mockApiHead = vi.fn();
vi.mock('$lib/api/client', () => ({
	api: {
		global: {
			get: (...args: unknown[]) => mockApiGet(...args),
			head: (...args: unknown[]) => mockApiHead(...args)
		}
	},
	ApiError: class extends Error {
		status: number;
		code: string;
		details: unknown;
		constructor(s: number, c: string, m: string, d?: unknown) {
			super(m);
			this.status = s;
			this.code = c;
			this.details = d;
		}
	}
}));

import { playerStore } from './player.svelte';
import { playbackToast } from '$lib/stores/playbackToast.svelte';

function makeItem(overrides: Partial<QueueItem> = {}): QueueItem {
	const id = overrides.trackSourceId ?? `vid-${Math.random().toString(36).slice(2, 6)}`;
	return {
		trackSourceId: id,
		trackName: overrides.trackName ?? 'Test Track',
		artistName: overrides.artistName ?? 'Test Artist',
		trackNumber: overrides.trackNumber ?? 1,
		albumId: overrides.albumId ?? 'album-1',
		albumName: overrides.albumName ?? 'Test Album',
		coverUrl: overrides.coverUrl ?? null,
		sourceType: overrides.sourceType ?? 'local',
		streamUrl: overrides.streamUrl ?? 'http://localhost/test.mp3',
		availableSources: overrides.availableSources ?? ['local', 'jellyfin'],
		sourceIds: overrides.sourceIds ?? { local: id, jellyfin: id },
		duration: overrides.duration,
		...overrides
	};
}

function makeItems(count: number): QueueItem[] {
	return Array.from({ length: count }, (_, i) =>
		makeItem({ trackSourceId: `vid-${i}`, trackName: `Track ${i + 1}`, trackNumber: i + 1 })
	);
}

describe('playerStore queue methods', () => {
	beforeEach(() => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
	});

	describe('addToQueue', () => {
		it('starts playback when queue is empty', () => {
			const item = makeItem();
			playerStore.addToQueue(item);
			expect(playerStore.queue).toHaveLength(1);
			expect(playerStore.queue[0].trackSourceId).toBe(item.trackSourceId);
			expect(playerStore.isPlayerVisible).toBe(true);
		});

		it('appends to end of queue when queue has items', () => {
			playerStore.playQueue(makeItems(2));
			const newItem = makeItem({ trackName: 'New Track' });
			playerStore.addToQueue(newItem);
			expect(playerStore.queue).toHaveLength(3);
			expect(playerStore.queue[2].trackName).toBe('New Track');
		});

		it('updates shuffle order when shuffle is enabled', () => {
			playerStore.playQueue(makeItems(2), 0, true);
			const initialShuffleLen = playerStore.shuffleOrder.length;
			playerStore.addToQueue(makeItem());
			expect(playerStore.shuffleOrder).toHaveLength(initialShuffleLen + 1);
		});
	});

	describe('playNext', () => {
		it('starts playback when queue is empty', () => {
			const item = makeItem();
			playerStore.playNext(item);
			expect(playerStore.queue).toHaveLength(1);
			expect(playerStore.isPlayerVisible).toBe(true);
		});

		it('inserts after current index', () => {
			playerStore.playQueue(makeItems(3));
			const newItem = makeItem({ trackName: 'Inserted' });
			playerStore.playNext(newItem);
			expect(playerStore.queue[1].trackName).toBe('Inserted');
			expect(playerStore.queue).toHaveLength(4);
		});
	});

	describe('addMultipleToQueue', () => {
		it('does nothing for empty array', () => {
			playerStore.addMultipleToQueue([]);
			expect(playerStore.queue).toHaveLength(0);
		});

		it('starts playback when queue is empty', () => {
			const items = makeItems(3);
			playerStore.addMultipleToQueue(items);
			expect(playerStore.queue).toHaveLength(3);
			expect(playerStore.isPlayerVisible).toBe(true);
		});

		it('appends all items to existing queue', () => {
			playerStore.playQueue(makeItems(2));
			playerStore.addMultipleToQueue(makeItems(3));
			expect(playerStore.queue).toHaveLength(5);
		});
	});

	describe('playMultipleNext', () => {
		it('does nothing for empty array', () => {
			playerStore.playMultipleNext([]);
			expect(playerStore.queue).toHaveLength(0);
		});

		it('starts playback when queue is empty', () => {
			const items = makeItems(2);
			playerStore.playMultipleNext(items);
			expect(playerStore.queue).toHaveLength(2);
			expect(playerStore.isPlayerVisible).toBe(true);
		});

		it('inserts all items after current index', () => {
			playerStore.playQueue(makeItems(3));
			const newItems = makeItems(2).map((item, i) => ({ ...item, trackName: `Inserted ${i}` }));
			playerStore.playMultipleNext(newItems);
			expect(playerStore.queue).toHaveLength(5);
			expect(playerStore.queue[1].trackName).toBe('Inserted 0');
			expect(playerStore.queue[2].trackName).toBe('Inserted 1');
		});
	});

	describe('removeFromQueue', () => {
		it('ignores out-of-bounds index', () => {
			playerStore.playQueue(makeItems(2));
			playerStore.removeFromQueue(-1);
			expect(playerStore.queue).toHaveLength(2);
			playerStore.removeFromQueue(10);
			expect(playerStore.queue).toHaveLength(2);
		});

		it('stops playback when removing only item', () => {
			playerStore.addToQueue(makeItem());
			playerStore.removeFromQueue(0);
			expect(playerStore.queue).toHaveLength(0);
			expect(playerStore.isPlayerVisible).toBe(false);
		});

		it('decrements currentIndex when removing item before current', () => {
			playerStore.playQueue(makeItems(3), 2);
			const prevIndex = playerStore.currentIndex;
			playerStore.removeFromQueue(0);
			expect(playerStore.currentIndex).toBe(prevIndex - 1);
			expect(playerStore.queue).toHaveLength(2);
		});

		it('updates shuffle order after removal', () => {
			playerStore.playQueue(makeItems(4), 0, true);
			const initialLen = playerStore.shuffleOrder.length;
			playerStore.removeFromQueue(3);
			expect(playerStore.shuffleOrder).toHaveLength(initialLen - 1);
		});
	});

	describe('reorderQueue', () => {
		it('does nothing for same index', () => {
			playerStore.playQueue(makeItems(3));
			const before = [...playerStore.queue];
			playerStore.reorderQueue(0, 0);
			expect(playerStore.queue.map((i) => i.trackSourceId)).toEqual(
				before.map((i) => i.trackSourceId)
			);
		});

		it('does nothing for out-of-bounds indices', () => {
			playerStore.playQueue(makeItems(3));
			const before = [...playerStore.queue];
			playerStore.reorderQueue(-1, 2);
			expect(playerStore.queue.map((i) => i.trackSourceId)).toEqual(
				before.map((i) => i.trackSourceId)
			);
		});

		it('moves an item forward', () => {
			playerStore.playQueue(makeItems(4));
			const moved = playerStore.queue[0].trackSourceId;
			playerStore.reorderQueue(0, 2);
			expect(playerStore.queue[2].trackSourceId).toBe(moved);
		});

		it('moves an item backward', () => {
			playerStore.playQueue(makeItems(4));
			const moved = playerStore.queue[3].trackSourceId;
			playerStore.reorderQueue(3, 1);
			expect(playerStore.queue[1].trackSourceId).toBe(moved);
		});

		it('tracks currentIndex when current item is moved', () => {
			playerStore.playQueue(makeItems(4));
			expect(playerStore.currentIndex).toBe(0);
			playerStore.reorderQueue(0, 3);
			expect(playerStore.currentIndex).toBe(3);
		});

		it('adjusts currentIndex when item moves across it', () => {
			playerStore.playQueue(makeItems(5), 2);
			expect(playerStore.currentIndex).toBe(2);
			playerStore.reorderQueue(0, 4);
			expect(playerStore.currentIndex).toBe(1);
		});

		it('updates shuffle order after reorder', () => {
			playerStore.playQueue(makeItems(4), 0, true);
			const initialOrder = [...playerStore.shuffleOrder];
			playerStore.reorderQueue(0, 3);
			expect(playerStore.shuffleOrder).toHaveLength(initialOrder.length);
		});
	});

	describe('clearQueue', () => {
		it('keeps current track and removes all upcoming tracks', () => {
			playerStore.playQueue(makeItems(3), 1);
			playerStore.clearQueue();
			expect(playerStore.queue).toHaveLength(1);
			expect(playerStore.queue[0].trackSourceId).toBe('vid-1');
			expect(playerStore.isPlayerVisible).toBe(true);
			expect(playerStore.currentIndex).toBe(0);
			expect(playerStore.upcomingQueueLength).toBe(0);
		});
	});

	describe('upcomingQueueLength', () => {
		it('counts tracks after current index for normal queue', () => {
			playerStore.playQueue(makeItems(3), 0);
			expect(playerStore.upcomingQueueLength).toBe(2);
		});

		it('counts remaining tracks in shuffle order', () => {
			playerStore.playQueue(makeItems(4), 0, true);
			playerStore.jumpToTrack(2);
			const expectedRemaining = Math.max(
				0,
				playerStore.shuffleOrder.length -
					playerStore.shuffleOrder.indexOf(playerStore.currentIndex) -
					1
			);
			expect(playerStore.upcomingQueueLength).toBe(expectedRemaining);
		});
	});

	describe('changeTrackSource', () => {
		it('ignores out-of-bounds index', () => {
			playerStore.playQueue(makeItems(3));
			const before = playerStore.queue[0].sourceType;
			playerStore.changeTrackSource(-1, 'jellyfin');
			expect(playerStore.queue[0].sourceType).toBe(before);
		});

		it('is a no-op on current track with toast', () => {
			playerStore.playQueue(makeItems(3));
			const currentIdx = playerStore.currentIndex;
			const beforeSource = playerStore.queue[currentIdx].sourceType;
			playerStore.changeTrackSource(currentIdx, 'jellyfin');
			expect(playerStore.queue[currentIdx].sourceType).toBe(beforeSource);
			expect(playbackToast.show).toHaveBeenCalled();
		});

		it('updates source on non-current item', () => {
			playerStore.playQueue(makeItems(3));
			playerStore.changeTrackSource(2, 'jellyfin');
			expect(playerStore.queue[2].sourceType).toBe('jellyfin');
		});

		it('updates streamUrl for target source', () => {
			playerStore.playQueue(makeItems(3));
			playerStore.changeTrackSource(1, 'jellyfin');
			expect(playerStore.queue[1].streamUrl).toBe('/api/v1/stream/jellyfin/vid-1');
		});
	});

	describe('updateQueueItemByPlaylistTrackId', () => {
		it('updates queued item matching playlistTrackId', () => {
			expect.assertions(3);
			const items = makeItems(3).map((item, i) => ({ ...item, playlistTrackId: `pt-${i}` }));
			playerStore.playQueue(items);
			playerStore.updateQueueItemByPlaylistTrackId('pt-2', 'jellyfin', 'jf-new', 'opus');
			expect(playerStore.queue[2].sourceType).toBe('jellyfin');
			expect(playerStore.queue[2].trackSourceId).toBe('jf-new');
			expect(playerStore.queue[2].streamUrl).toBe('/api/v1/stream/jellyfin/jf-new');
		});

		it('is a no-op when playlistTrackId is not found', () => {
			expect.assertions(1);
			const items = makeItems(3).map((item, i) => ({ ...item, playlistTrackId: `pt-${i}` }));
			playerStore.playQueue(items);
			const before = playerStore.queue[1].sourceType;
			playerStore.updateQueueItemByPlaylistTrackId('pt-nonexistent', 'jellyfin', 'jf-new');
			expect(playerStore.queue[1].sourceType).toBe(before);
		});

		it('skips currently playing track', () => {
			expect.assertions(1);
			const items = makeItems(3).map((item, i) => ({ ...item, playlistTrackId: `pt-${i}` }));
			playerStore.playQueue(items);
			const currentIdx = playerStore.currentIndex;
			const before = playerStore.queue[currentIdx].sourceType;
			playerStore.updateQueueItemByPlaylistTrackId(`pt-${currentIdx}`, 'jellyfin', 'jf-new');
			expect(playerStore.queue[currentIdx].sourceType).toBe(before);
		});

		it('resolves local streamUrl correctly', () => {
			expect.assertions(1);
			const items = makeItems(3).map((item, i) => ({ ...item, playlistTrackId: `pt-${i}` }));
			playerStore.playQueue(items);
			playerStore.updateQueueItemByPlaylistTrackId('pt-1', 'local', '999', 'flac');
			expect(playerStore.queue[1].streamUrl).toBe('/api/v1/stream/local/999');
		});
	});

	describe('session migration', () => {
		it('maps legacy howler source type to local during resume', () => {
			const legacySession = {
				nowPlaying: {
					albumId: 'album-1',
					albumName: 'Album',
					artistName: 'Artist',
					coverUrl: null,
					sourceType: 'howler',
					trackSourceId: '1',
					trackName: 'Track'
				},
				queue: [
					{
						trackSourceId: '1',
						trackName: 'Track',
						artistName: 'Artist',
						trackNumber: 1,
						albumId: 'album-1',
						albumName: 'Album',
						coverUrl: null,
						sourceType: 'howler',
						streamUrl: '/api/v1/stream/local/1',
						availableSources: ['howler', 'jellyfin']
					}
				],
				currentIndex: 0,
				progress: 0,
				shuffleEnabled: false,
				shuffleOrder: []
			};

			localStorage.setItem('droppedneedle_player_session', JSON.stringify(legacySession));
			playerStore.resumeSession();

			expect(playerStore.queue[0].sourceType).toBe('local');
			expect(playerStore.queue[0].availableSources).toEqual(['local', 'jellyfin']);
		});
	});

	describe('playQueue', () => {
		it('does nothing for empty array', () => {
			playerStore.playQueue([]);
			expect(playerStore.queue).toHaveLength(0);
		});

		it('sets queue and starts playback at specified index', () => {
			const items = makeItems(5);
			playerStore.playQueue(items, 2);
			expect(playerStore.queue).toHaveLength(5);
			expect(playerStore.currentIndex).toBe(2);
			expect(playerStore.isPlayerVisible).toBe(true);
		});

		it('creates shuffle order when shuffle enabled', () => {
			playerStore.playQueue(makeItems(5), 0, true);
			expect(playerStore.shuffleEnabled).toBe(true);
			expect(playerStore.shuffleOrder).toHaveLength(5);
		});

		it('does not create shuffle order when disabled', () => {
			playerStore.playQueue(makeItems(5), 0, false);
			expect(playerStore.shuffleEnabled).toBe(false);
		});
	});

	describe('toggleShuffle', () => {
		it('enables shuffle and creates order', () => {
			playerStore.playQueue(makeItems(4));
			playerStore.toggleShuffle();
			expect(playerStore.shuffleEnabled).toBe(true);
			expect(playerStore.shuffleOrder).toHaveLength(4);
			expect(playerStore.shuffleOrder[0]).toBe(playerStore.currentIndex);
		});

		it('disables shuffle and clears order', () => {
			playerStore.playQueue(makeItems(4), 0, true);
			playerStore.toggleShuffle();
			expect(playerStore.shuffleEnabled).toBe(false);
			expect(playerStore.shuffleOrder).toHaveLength(0);
		});

		it('only shuffles upcoming tracks, not played ones', () => {
			playerStore.playQueue(makeItems(6), 0);
			playerStore.jumpToTrack(3);

			playerStore.toggleShuffle();

			expect(playerStore.shuffleOrder).toHaveLength(6);
			expect(playerStore.shuffleOrder.slice(0, 3)).toEqual([0, 1, 2]);
			expect(playerStore.shuffleOrder[3]).toBe(3);
			const upcomingPart = playerStore.shuffleOrder.slice(4);
			expect(upcomingPart.sort()).toEqual([4, 5]);
		});

		it('preserves played order when toggled at end of queue', () => {
			playerStore.playQueue(makeItems(4), 0);
			playerStore.jumpToTrack(3);

			playerStore.toggleShuffle();
			expect(playerStore.shuffleOrder.slice(0, 3)).toEqual([0, 1, 2]);
			expect(playerStore.shuffleOrder[3]).toBe(3);
		});

		it('includes all indices when toggled at start', () => {
			playerStore.playQueue(makeItems(5));
			playerStore.toggleShuffle();

			expect(playerStore.shuffleOrder).toHaveLength(5);
			expect(playerStore.shuffleOrder[0]).toBe(0);
			expect([...playerStore.shuffleOrder].sort()).toEqual([0, 1, 2, 3, 4]);
		});
	});

	describe('queueOrigin tagging', () => {
		it('playQueue stamps items as context', () => {
			playerStore.playQueue(makeItems(3));
			expect(playerStore.queue.every((i) => i.queueOrigin === 'context')).toBe(true);
		});

		it('addToQueue stamps item as manual', () => {
			playerStore.playQueue(makeItems(2));
			playerStore.addToQueue(makeItem({ trackName: 'Manual' }));
			expect(playerStore.queue[2].queueOrigin).toBe('manual');
		});

		it('addMultipleToQueue stamps items as manual', () => {
			playerStore.playQueue(makeItems(2));
			playerStore.addMultipleToQueue(makeItems(2));
			expect(playerStore.queue[2].queueOrigin).toBe('manual');
			expect(playerStore.queue[3].queueOrigin).toBe('manual');
		});

		it('playNext stamps item as manual', () => {
			playerStore.playQueue(makeItems(2));
			playerStore.playNext(makeItem({ trackName: 'Next' }));
			expect(playerStore.queue[1].queueOrigin).toBe('manual');
		});

		it('playMultipleNext stamps items as manual', () => {
			playerStore.playQueue(makeItems(2));
			playerStore.playMultipleNext(makeItems(2));
			expect(playerStore.queue[1].queueOrigin).toBe('manual');
			expect(playerStore.queue[2].queueOrigin).toBe('manual');
		});

		it('does not overwrite existing context origin on playQueue', () => {
			const items = makeItems(2);
			items[0].queueOrigin = 'manual';
			playerStore.playQueue(items);
			expect(playerStore.queue[0].queueOrigin).toBe('context');
		});
	});

	describe('played track cleanup', () => {
		it('removes manual tracks after advancing past them', async () => {
			playerStore.playQueue(makeItems(2));
			playerStore.playNext(makeItem({ trackName: 'Manual Next' }));
			expect(playerStore.queue).toHaveLength(3);

			playerStore.jumpToTrack(1);
			playerStore.nextTrack();
			await vi.waitFor(() => {
				expect(playerStore.queue.find((i) => i.trackName === 'Manual Next')).toBeUndefined();
			});
		});

		it('keeps context tracks up to history cap', async () => {
			playerStore.playQueue(makeItems(6));

			playerStore.nextTrack();
			await vi.waitFor(() => {
				expect(playerStore.currentIndex).toBeGreaterThan(0);
			});
			playerStore.nextTrack();
			await vi.waitFor(() => {
				expect(playerStore.currentIndex).toBeGreaterThan(0);
			});
			playerStore.nextTrack();
			await vi.waitFor(() => {
				expect(playerStore.currentIndex).toBeGreaterThan(0);
			});
			playerStore.nextTrack();
			await vi.waitFor(() => {
				expect(playerStore.currentIndex).toBeGreaterThan(0);
			});
			playerStore.nextTrack();
			await vi.waitFor(() => {
				const playedCount = playerStore.currentIndex;
				expect(playedCount).toBeLessThanOrEqual(3);
			});
		});

		it('trims oldest context tracks beyond history cap', async () => {
			playerStore.playQueue(makeItems(8));

			for (let i = 0; i < 6; i++) {
				playerStore.nextTrack();
				await vi.waitFor(() => {
					expect(playerStore.currentIndex).toBeGreaterThan(0);
				});
			}

			const historyBehind = playerStore.currentIndex;
			expect(historyBehind).toBeLessThanOrEqual(3);
		});

		it('does not remove tracks when at start of queue', async () => {
			playerStore.playQueue(makeItems(3));
			playerStore.nextTrack();
			await vi.waitFor(() => {
				expect(playerStore.currentIndex).toBeGreaterThanOrEqual(0);
			});
			expect(playerStore.queue.length).toBeGreaterThanOrEqual(2);
		});
	});

	describe('session migration with queueOrigin', () => {
		it('defaults missing queueOrigin to context during resume', () => {
			const session = {
				nowPlaying: {
					albumId: 'album-1',
					albumName: 'Album',
					artistName: 'Artist',
					coverUrl: null,
					sourceType: 'local',
					trackSourceId: '1',
					trackName: 'Track'
				},
				queue: [
					{
						trackSourceId: '1',
						trackName: 'Track',
						artistName: 'Artist',
						trackNumber: 1,
						albumId: 'album-1',
						albumName: 'Album',
						coverUrl: null,
						sourceType: 'local',
						streamUrl: '/api/v1/stream/local/1',
						availableSources: ['local']
					}
				],
				currentIndex: 0,
				progress: 0,
				shuffleEnabled: false,
				shuffleOrder: []
			};

			localStorage.setItem('droppedneedle_player_session', JSON.stringify(session));
			playerStore.resumeSession();

			expect(playerStore.queue[0].queueOrigin).toBe('context');
		});

		it('preserves existing queueOrigin during resume', () => {
			const session = {
				nowPlaying: {
					albumId: 'album-1',
					albumName: 'Album',
					artistName: 'Artist',
					coverUrl: null,
					sourceType: 'local',
					trackSourceId: '1',
					trackName: 'Track'
				},
				queue: [
					{
						trackSourceId: '1',
						trackName: 'Track',
						artistName: 'Artist',
						trackNumber: 1,
						albumId: 'album-1',
						albumName: 'Album',
						coverUrl: null,
						sourceType: 'local',
						streamUrl: '/api/v1/stream/local/1',
						availableSources: ['local'],
						queueOrigin: 'manual'
					}
				],
				currentIndex: 0,
				progress: 0,
				shuffleEnabled: false,
				shuffleOrder: []
			};

			localStorage.setItem('droppedneedle_player_session', JSON.stringify(session));
			playerStore.resumeSession();

			expect(playerStore.queue[0].queueOrigin).toBe('manual');
		});
	});

	describe('jumpToTrack', () => {
		it('loads the track at the specified index', () => {
			playerStore.playQueue(makeItems(3));
			playerStore.jumpToTrack(2);
			expect(playerStore.currentIndex).toBe(2);
		});

		it('ignores out-of-bounds index', () => {
			playerStore.playQueue(makeItems(3));
			const before = playerStore.currentIndex;
			playerStore.jumpToTrack(10);
			expect(playerStore.currentIndex).toBe(before);
		});
	});
});

describe('user intent gating (pause/stop vs auto-recovery)', () => {
	beforeEach(() => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
		vi.useFakeTimers();
		mockApiHead.mockResolvedValue(new Response(null, { status: 200 }));
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('playQueue marks playback as intended', async () => {
		playerStore.playQueue(makeItems(3));
		await vi.advanceTimersByTimeAsync(0);

		expect(playerStore.intendedPlaying).toBe(true);
	});

	it('pause clears intent; play/togglePlay restore it', async () => {
		playerStore.playQueue(makeItems(3));
		await vi.advanceTimersByTimeAsync(0);

		playerStore.pause();
		expect(playerStore.intendedPlaying).toBe(false);

		playerStore.play();
		expect(playerStore.intendedPlaying).toBe(true);

		capturedStateCallbacks.forEach((cb) => cb('playing'));
		playerStore.togglePlay();
		expect(playerStore.intendedPlaying).toBe(false);
	});

	it('auto-skips on error while playback is intended', async () => {
		playerStore.playQueue(makeItems(3));
		await vi.advanceTimersByTimeAsync(0);

		capturedErrorCallbacks.forEach((cb) => cb({ code: 'NETWORK_STALL', message: 'stall' }));
		await vi.advanceTimersByTimeAsync(2000);

		expect(playerStore.currentIndex).toBe(1);
	});

	it('does NOT auto-skip on error after the user pauses', async () => {
		playerStore.playQueue(makeItems(3));
		await vi.advanceTimersByTimeAsync(0);
		playerStore.pause();

		capturedErrorCallbacks.forEach((cb) => cb({ code: 'NETWORK_STALL', message: 'stall' }));
		await vi.advanceTimersByTimeAsync(10_000);

		expect(playerStore.currentIndex).toBe(0);
		expect(playerStore.playbackState).toBe('error');
		expect(playbackToast.show).not.toHaveBeenCalled();
	});

	it('stop() clears intent, destroys the source, and empties the queue', async () => {
		playerStore.playQueue(makeItems(3));
		await vi.advanceTimersByTimeAsync(0);
		const source = playerStore.currentSource;
		expect(source).not.toBeNull();

		playerStore.stop();
		await vi.advanceTimersByTimeAsync(10_000);

		expect(playerStore.intendedPlaying).toBe(false);
		expect(source!.destroy).toHaveBeenCalled();
		expect(playerStore.currentSource).toBeNull();
		expect(playerStore.queue).toHaveLength(0);
	});

	it('an error landing after stop() cannot resurrect playback', async () => {
		playerStore.playQueue(makeItems(3));
		await vi.advanceTimersByTimeAsync(0);
		const staleErrorCallbacks = [...capturedErrorCallbacks];

		playerStore.stop();
		staleErrorCallbacks.forEach((cb) => cb({ code: 'NETWORK_STALL', message: 'stall' }));
		await vi.advanceTimersByTimeAsync(10_000);

		expect(playerStore.queue).toHaveLength(0);
		expect(playerStore.currentSource).toBeNull();
		expect(playerStore.intendedPlaying).toBe(false);
	});
});

describe('Jellyfin session lifecycle', () => {
	let jellyfinApi: {
		startSession: ReturnType<typeof vi.fn>;
		reportProgress: ReturnType<typeof vi.fn>;
		reportStop: ReturnType<typeof vi.fn>;
	};

	beforeEach(async () => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
		vi.useFakeTimers();

		jellyfinApi =
			(await import('$lib/player/jellyfinPlaybackApi')) as unknown as typeof jellyfinApi;
		jellyfinApi.startSession.mockResolvedValue('ps-123');

		mockApiGet.mockResolvedValue({
			url: 'http://jf/Audio/1/stream?static=true',
			seekable: true,
			playSessionId: 'ps-123'
		});
		mockApiHead.mockResolvedValue(new Response(null, { status: 200 }));
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
		vi.stubGlobal('localStorage', {
			getItem: vi.fn((key: string) => (storage.has(key) ? storage.get(key)! : null)),
			setItem: vi.fn((key: string, value: string) => {
				storage.set(key, value);
			}),
			removeItem: vi.fn((key: string) => {
				storage.delete(key);
			}),
			clear: vi.fn(() => {
				storage.clear();
			})
		});
	});

	function makeJellyfinItem(overrides: Partial<QueueItem> = {}): QueueItem {
		return makeItem({
			sourceType: 'jellyfin',
			trackSourceId: 'jf-1',
			streamUrl: undefined,
			...overrides
		});
	}

	it('calls startSession when a Jellyfin track is loaded', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		expect(jellyfinApi.startSession).toHaveBeenCalledWith('jf-1', undefined);
	});

	it('calls reportStop when switching tracks', async () => {
		playerStore.playQueue([
			makeJellyfinItem({ trackSourceId: 'jf-1' }),
			makeItem({ trackSourceId: 'loc-2' })
		]);
		await vi.advanceTimersByTimeAsync(0);

		capturedStateCallbacks.forEach((cb) => cb('playing'));
		capturedProgressCallbacks.forEach((cb) => cb(30, 180));
		await vi.advanceTimersByTimeAsync(0);

		playerStore.nextTrack();
		await vi.advanceTimersByTimeAsync(0);

		expect(jellyfinApi.reportStop).toHaveBeenCalledWith('jf-1', 'ps-123', expect.any(Number));
	});

	it('calls reportStop when stop() is called', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		playerStore.stop();
		await vi.advanceTimersByTimeAsync(0);

		expect(jellyfinApi.reportStop).toHaveBeenCalledWith('jf-1', 'ps-123', expect.any(Number));
	});

	it('calls reportProgress during the progress interval', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		capturedStateCallbacks.forEach((cb) => cb('playing'));
		capturedProgressCallbacks.forEach((cb) => cb(10, 180));
		await vi.advanceTimersByTimeAsync(0);

		vi.advanceTimersByTime(10_000);

		expect(jellyfinApi.reportProgress).toHaveBeenCalledWith(
			'jf-1',
			'ps-123',
			expect.any(Number),
			false
		);
	});
});

describe('beforeunload beacon', () => {
	let addEventListenerSpy: ReturnType<typeof vi.fn>;
	let removeEventListenerSpy: ReturnType<typeof vi.fn>;
	let sendBeaconMock: ReturnType<typeof vi.fn>;
	let jellyfinApi: {
		startSession: ReturnType<typeof vi.fn>;
		reportProgress: ReturnType<typeof vi.fn>;
		reportStop: ReturnType<typeof vi.fn>;
	};

	beforeEach(async () => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
		vi.useFakeTimers();

		jellyfinApi =
			(await import('$lib/player/jellyfinPlaybackApi')) as unknown as typeof jellyfinApi;
		jellyfinApi.startSession.mockResolvedValue('ps-beacon');

		mockApiGet.mockResolvedValue({
			url: 'http://jf/Audio/1/stream?static=true',
			seekable: true,
			playSessionId: 'ps-beacon'
		});
		mockApiHead.mockResolvedValue(new Response(null, { status: 200 }));

		const listeners = new Map<string, Set<Function>>();
		const windowStub = {
			addEventListener: vi.fn((event: string, handler: Function) => {
				const set = listeners.get(event) ?? new Set();
				set.add(handler);
				listeners.set(event, set);
			}),
			removeEventListener: vi.fn((event: string, handler: Function) => {
				listeners.get(event)?.delete(handler);
			})
		};
		vi.stubGlobal('window', windowStub);
		addEventListenerSpy = windowStub.addEventListener;
		removeEventListenerSpy = windowStub.removeEventListener;

		sendBeaconMock = vi.fn(() => true);
		vi.stubGlobal('navigator', { sendBeacon: sendBeaconMock });
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
		vi.stubGlobal('localStorage', {
			getItem: vi.fn((key: string) => (storage.has(key) ? storage.get(key)! : null)),
			setItem: vi.fn((key: string, value: string) => {
				storage.set(key, value);
			}),
			removeItem: vi.fn((key: string) => {
				storage.delete(key);
			}),
			clear: vi.fn(() => {
				storage.clear();
			})
		});
	});

	function makeJellyfinItem(overrides: Partial<QueueItem> = {}): QueueItem {
		return makeItem({
			sourceType: 'jellyfin',
			trackSourceId: 'jf-beacon',
			streamUrl: undefined,
			...overrides
		});
	}

	it('registers beforeunload listener when a Jellyfin track starts', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		expect(addEventListenerSpy).toHaveBeenCalledWith('beforeunload', expect.any(Function));
	});

	it('sends beacon with correct payload on beforeunload', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		capturedProgressCallbacks.forEach((cb) => cb(45, 180));

		const beforeUnloadHandler = addEventListenerSpy.mock.calls.find(
			(call) => call[0] === 'beforeunload'
		)?.[1] as (() => void) | undefined;

		expect(beforeUnloadHandler).toBeDefined();
		beforeUnloadHandler!();

		expect(sendBeaconMock).toHaveBeenCalledWith(
			'/api/v1/stream/jellyfin/jf-beacon/stop',
			expect.any(Blob)
		);

		const sentBlob = sendBeaconMock.mock.calls[0][1] as Blob;
		expect(sentBlob.type).toBe('application/json');
		const text = await sentBlob.text();
		const parsed = JSON.parse(text);
		expect(parsed).toEqual({ play_session_id: 'ps-beacon', position_seconds: 45 });
	});

	it('removes beforeunload listener on destroy/stop', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		playerStore.stop();
		await vi.advanceTimersByTimeAsync(0);

		expect(removeEventListenerSpy).toHaveBeenCalledWith('beforeunload', expect.any(Function));
	});
});

describe('non-seekable state propagation', () => {
	beforeEach(() => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
		vi.useFakeTimers();

		mockApiGet.mockResolvedValue({
			url: 'http://jf/Audio/1/universal?transcode',
			seekable: false,
			playSessionId: 'ps-ns'
		});
		mockApiHead.mockResolvedValue(new Response(null, { status: 200 }));
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
		vi.stubGlobal('localStorage', {
			getItem: vi.fn((key: string) => (storage.has(key) ? storage.get(key)! : null)),
			setItem: vi.fn((key: string, value: string) => {
				storage.set(key, value);
			}),
			removeItem: vi.fn((key: string) => {
				storage.delete(key);
			}),
			clear: vi.fn(() => {
				storage.clear();
			})
		});
	});

	it('sets isSeekable to true for Jellyfin streams', async () => {
		const item = makeItem({ sourceType: 'jellyfin', trackSourceId: 'jf-ns', streamUrl: undefined });
		playerStore.playQueue([item]);
		await vi.advanceTimersByTimeAsync(0);

		expect(playerStore.isSeekable).toBe(true);
	});
});
