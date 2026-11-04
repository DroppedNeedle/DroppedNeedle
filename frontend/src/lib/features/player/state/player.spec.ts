/* eslint-disable @typescript-eslint/no-unsafe-function-type */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { QueueItem } from '$lib/features/player/playback/types';

type StateCallback = (state: import('$lib/features/player/playback/types').PlaybackState) => void;
type ProgressCallback = (currentTime: number, duration: number) => void;
type ErrorCallback = (error: { code: string; message: string }) => void;

let capturedStateCallbacks: StateCallback[] = [];
let capturedProgressCallbacks: ProgressCallback[] = [];
let capturedErrorCallbacks: ErrorCallback[] = [];

vi.mock('$lib/features/player/playback/createSource', () => ({
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

vi.mock('$lib/features/player/playback/playbackGateway', async (importActual) => ({
	...(await importActual<typeof import('$lib/features/player/playback/playbackGateway')>()),
	startPlaybackSession: vi.fn(async () => ({ session: 'mock-session' })),
	reportPlaybackProgress: vi.fn(async () => undefined),
	stopPlaybackSession: vi.fn(async () => undefined)
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

vi.mock('$lib/stores/toast', () => ({
	toastStore: {
		notify: vi.fn(),
		hide: vi.fn(),
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
		get: (...args: unknown[]) => mockApiGet(...args),
		head: (...args: unknown[]) => mockApiHead(...args)
	}
}));

import { playerStore } from './player.svelte';
import { toastStore } from '$lib/stores/toast';
import { radioSession } from '$lib/features/player/state/radioSession.svelte';
import { authStore } from '$lib/stores/authStore.svelte';
import {
	startPlaybackSession,
	stopPlaybackSession
} from '$lib/features/player/playback/playbackGateway';
import { streamUrlFor } from '$lib/features/player/playback/streamUrl';
import { createPlaybackSource } from '$lib/features/player/playback/createSource';

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

function resetPlayer(): void {
	localStorage.clear();
	playerStore.stop();
	vi.clearAllMocks();
}

describe('playerStore queue methods', () => {
	beforeEach(resetPlayer);

	describe('queue operations', () => {
		it('adds, inserts, removes, reorders and clears tracks, keeping the current index', () => {
			{
				// Appends to end of queue when queue has items.
				resetPlayer();
				playerStore.playQueue(makeItems(2));
				const newItem = makeItem({ trackName: 'New Track' });
				playerStore.addToQueue(newItem);
				expect(playerStore.queue).toHaveLength(3);
				expect(playerStore.queue[2].trackName).toBe('New Track');
			}

			{
				// Inserts after current index.
				resetPlayer();
				playerStore.playQueue(makeItems(3));
				const newItem = makeItem({ trackName: 'Inserted' });
				playerStore.playNext(newItem);
				expect(playerStore.queue[1].trackName).toBe('Inserted');
				expect(playerStore.queue).toHaveLength(4);
			}

			{
				// Appends all items to existing queue.
				resetPlayer();
				playerStore.playQueue(makeItems(2));
				playerStore.addMultipleToQueue(makeItems(3));
				expect(playerStore.queue).toHaveLength(5);
			}

			{
				// Inserts all items after current index.
				resetPlayer();
				playerStore.playQueue(makeItems(3));
				const newItems = makeItems(2).map((item, i) => ({ ...item, trackName: `Inserted ${i}` }));
				playerStore.playMultipleNext(newItems);
				expect(playerStore.queue).toHaveLength(5);
				expect(playerStore.queue[1].trackName).toBe('Inserted 0');
				expect(playerStore.queue[2].trackName).toBe('Inserted 1');
			}

			{
				// Stops playback when removing only item.
				resetPlayer();
				playerStore.addToQueue(makeItem());
				playerStore.removeFromQueue(0);
				expect(playerStore.queue).toHaveLength(0);
				expect(playerStore.isPlayerVisible).toBe(false);
			}

			{
				// Decrements currentIndex when removing item before current.
				resetPlayer();
				playerStore.playQueue(makeItems(3), 2);
				const prevIndex = playerStore.currentIndex;
				playerStore.removeFromQueue(0);
				expect(playerStore.currentIndex).toBe(prevIndex - 1);
				expect(playerStore.queue).toHaveLength(2);
			}

			{
				// Tracks currentIndex when current item is moved.
				resetPlayer();
				playerStore.playQueue(makeItems(4));
				expect(playerStore.currentIndex).toBe(0);
				playerStore.reorderQueue(0, 3);
				expect(playerStore.currentIndex).toBe(3);
			}

			{
				// Adjusts currentIndex when item moves across it.
				resetPlayer();
				playerStore.playQueue(makeItems(5), 2);
				expect(playerStore.currentIndex).toBe(2);
				playerStore.reorderQueue(0, 4);
				expect(playerStore.currentIndex).toBe(1);
			}

			{
				// Keeps current track and removes all upcoming tracks.
				resetPlayer();
				playerStore.playQueue(makeItems(3), 1);
				playerStore.clearQueue();
				expect(playerStore.queue).toHaveLength(1);
				expect(playerStore.queue[0].trackSourceId).toBe('vid-1');
				expect(playerStore.isPlayerVisible).toBe(true);
				expect(playerStore.currentIndex).toBe(0);
				expect(playerStore.upcomingQueueLength).toBe(0);
			}

			{
				// Counts remaining tracks in shuffle order.
				resetPlayer();
				playerStore.playQueue(makeItems(4), 0, true);
				playerStore.jumpToTrack(2);
				const expectedRemaining = Math.max(
					0,
					playerStore.shuffleOrder.length -
						playerStore.shuffleOrder.indexOf(playerStore.currentIndex) -
						1
				);
				expect(playerStore.upcomingQueueLength).toBe(expectedRemaining);
			}

			{
				// Sets queue and starts playback at specified index.
				resetPlayer();
				const items = makeItems(5);
				playerStore.playQueue(items, 2);
				expect(playerStore.queue).toHaveLength(5);
				expect(playerStore.currentIndex).toBe(2);
				expect(playerStore.isPlayerVisible).toBe(true);
			}
		});
	});

	describe('changeTrackSource', () => {
		it('switches the source of a queued track, never the one playing', () => {
			{
				// Is a no-op on current track with toast.
				resetPlayer();
				playerStore.playQueue(makeItems(3));
				const currentIdx = playerStore.currentIndex;
				const beforeSource = playerStore.queue[currentIdx].sourceType;
				playerStore.changeTrackSource(currentIdx, 'jellyfin');
				expect(playerStore.queue[currentIdx].sourceType).toBe(beforeSource);
				expect(toastStore.notify).toHaveBeenCalled();
			}

			{
				// Updates streamUrl for target source.
				resetPlayer();
				playerStore.playQueue(makeItems(3));
				playerStore.changeTrackSource(1, 'jellyfin');
				expect(streamUrlFor(playerStore.queue[1])).toBe('/api/v3/stream/jellyfin/vid-1');
			}
		});
	});

	describe('updateQueueItemByPlaylistTrackId', () => {
		it('updates a queued playlist entry, never the one playing', () => {
			{
				// Updates queued item matching playlistTrackId.
				resetPlayer();
				const items = makeItems(3).map((item, i) => ({ ...item, playlistTrackId: `pt-${i}` }));
				playerStore.playQueue(items);
				playerStore.updateQueueItemByPlaylistTrackId('pt-2', 'jellyfin', 'jf-new', 'opus');
				expect(playerStore.queue[2].sourceType).toBe('jellyfin');
				expect(playerStore.queue[2].trackSourceId).toBe('jf-new');
				expect(streamUrlFor(playerStore.queue[2])).toBe('/api/v3/stream/jellyfin/jf-new');
			}

			{
				// Skips currently playing track.
				resetPlayer();
				const items = makeItems(3).map((item, i) => ({ ...item, playlistTrackId: `pt-${i}` }));
				playerStore.playQueue(items);
				const currentIdx = playerStore.currentIndex;
				const before = playerStore.queue[currentIdx].sourceType;
				playerStore.updateQueueItemByPlaylistTrackId(`pt-${currentIdx}`, 'jellyfin', 'jf-new');
				expect(playerStore.queue[currentIdx].sourceType).toBe(before);
			}
		});
	});

	describe('toggleShuffle', () => {
		it('shuffles only upcoming tracks, keeping the played order', () => {
			{
				// Only shuffles upcoming tracks, not played ones.
				resetPlayer();
				playerStore.playQueue(makeItems(6), 0);
				playerStore.jumpToTrack(3);

				playerStore.toggleShuffle();

				expect(playerStore.shuffleOrder).toHaveLength(6);
				expect(playerStore.shuffleOrder.slice(0, 3)).toEqual([0, 1, 2]);
				expect(playerStore.shuffleOrder[3]).toBe(3);
				const upcomingPart = playerStore.shuffleOrder.slice(4);
				expect(upcomingPart.sort()).toEqual([4, 5]);
			}

			{
				// Preserves played order when toggled at end of queue.
				resetPlayer();
				playerStore.playQueue(makeItems(4), 0);
				playerStore.jumpToTrack(3);

				playerStore.toggleShuffle();
				expect(playerStore.shuffleOrder.slice(0, 3)).toEqual([0, 1, 2]);
				expect(playerStore.shuffleOrder[3]).toBe(3);
			}
		});
	});

	describe('queueOrigin tagging', () => {
		it('stamps what a queue started with as context and what was added as manual', () => {
			{
				// PlayQueue stamps items as context.
				resetPlayer();
				playerStore.playQueue(makeItems(3));
				expect(playerStore.queue.every((i) => i.queueOrigin === 'context')).toBe(true);
			}

			{
				// AddToQueue stamps item as manual.
				resetPlayer();
				playerStore.playQueue(makeItems(2));
				playerStore.addToQueue(makeItem({ trackName: 'Manual' }));
				expect(playerStore.queue[2].queueOrigin).toBe('manual');
			}

			{
				// Does not overwrite existing context origin on playQueue.
				resetPlayer();
				const items = makeItems(2);
				items[0].queueOrigin = 'manual';
				playerStore.playQueue(items);
				expect(playerStore.queue[0].queueOrigin).toBe('context');
			}
		});
	});

	describe('played track cleanup', () => {
		it('drops played manual tracks and keeps at most three played context tracks', async () => {
			{
				// Removes manual tracks after advancing past them.
				resetPlayer();
				playerStore.playQueue(makeItems(2));
				playerStore.playNext(makeItem({ trackName: 'Manual Next' }));
				expect(playerStore.queue).toHaveLength(3);

				playerStore.jumpToTrack(1);
				playerStore.nextTrack();
				await vi.waitFor(() => {
					expect(playerStore.queue.find((i) => i.trackName === 'Manual Next')).toBeUndefined();
				});
			}

			{
				// Keeps context tracks up to history cap.
				resetPlayer();
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
			}

			{
				// Trims oldest context tracks beyond history cap.
				resetPlayer();
				playerStore.playQueue(makeItems(8));

				for (let i = 0; i < 6; i++) {
					playerStore.nextTrack();
					await vi.waitFor(() => {
						expect(playerStore.currentIndex).toBeGreaterThan(0);
					});
				}

				const historyBehind = playerStore.currentIndex;
				expect(historyBehind).toBeLessThanOrEqual(3);
			}
		});
	});

	describe('session migration with queueOrigin', () => {
		it('defaults missing queueOrigin to context during resume', () => {
			authStore.setUser({
				id: 'user-1',
				display_name: 'User',
				role: 'user',
				email: null,
				avatar_url: null,
				username: 'user',
				username_display: 'User',
				providers: ['local']
			});
			const session = {
				userId: 'user-1',
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
						availableSources: ['local']
					}
				],
				currentIndex: 0,
				progress: 0,
				shuffleEnabled: false,
				shuffleOrder: []
			};

			localStorage.setItem('droppedneedle_player_session_v3', JSON.stringify(session));
			playerStore.resumeSession();

			expect(playerStore.queue[0].queueOrigin).toBe('context');
		});
	});

	describe('radio session lifecycle', () => {
		it('ends the station when the player closes, never on a pause', () => {
			{
				// Ends the station when the player is closed.
				resetPlayer();
				const launch = radioSession.beginLaunch();
				radioSession.start(launch.generation);
				playerStore.playQueue([makeItem({ playlistTrackId: 'radio:test artist|test track' })]);

				playerStore.stop();

				expect(radioSession.active).toBe(false);
				expect(playerStore.queue).toHaveLength(0);
			}

			{
				// Keeps the fixed station queue when playback is paused.
				resetPlayer();
				const launch = radioSession.beginLaunch();
				radioSession.start(launch.generation);
				playerStore.playQueue([makeItem({ playlistTrackId: 'radio:test artist|test track' })]);

				playerStore.pause();

				expect(radioSession.active).toBe(true);
				expect(playerStore.queue).toHaveLength(1);
			}
		});
	});
});

describe('Jellyfin session lifecycle', () => {
	beforeEach(() => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
		vi.useFakeTimers();

		mockApiGet.mockResolvedValue({
			url: 'http://jf/Audio/1/stream?static=true',
			seekable: true
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
			...overrides
		});
	}

	it('starts the session when a Jellyfin track plays, not when it loads', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);
		expect(startPlaybackSession).not.toHaveBeenCalled();

		capturedStateCallbacks.forEach((cb) => cb('playing'));
		await vi.advanceTimersByTimeAsync(0);

		expect(startPlaybackSession).toHaveBeenCalledWith({
			source: 'jellyfin',
			track_id: 'jf-1',
			position_ms: 0
		});
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

		expect(stopPlaybackSession).toHaveBeenCalledWith({
			source: 'jellyfin',
			track_id: 'jf-1',
			position_ms: expect.any(Number),
			start_position_ms: 0
		});
	});
});

describe('beforeunload beacon', () => {
	let addEventListenerSpy: ReturnType<typeof vi.fn>;
	let sendBeaconMock: ReturnType<typeof vi.fn>;

	beforeEach(() => {
		localStorage.clear();
		playerStore.stop();
		vi.clearAllMocks();
		vi.useFakeTimers();

		mockApiGet.mockResolvedValue({
			url: 'http://jf/Audio/1/stream?static=true',
			seekable: true
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
			...overrides
		});
	}

	it('sends beacon with correct payload on beforeunload', async () => {
		playerStore.playQueue([makeJellyfinItem()]);
		await vi.advanceTimersByTimeAsync(0);

		capturedStateCallbacks.forEach((cb) => cb('playing'));
		capturedProgressCallbacks.forEach((cb) => cb(45, 180));

		const beforeUnloadHandler = addEventListenerSpy.mock.calls.find(
			(call) => call[0] === 'beforeunload'
		)?.[1] as (() => void) | undefined;

		expect(beforeUnloadHandler).toBeDefined();
		beforeUnloadHandler!();

		expect(sendBeaconMock).toHaveBeenCalledWith('/api/v3/playback/stop', expect.any(Blob));

		const sentBlob = sendBeaconMock.mock.calls[0][1] as Blob;
		expect(sentBlob.type).toBe('application/json');
		const text = await sentBlob.text();
		const parsed = JSON.parse(text);
		expect(parsed).toEqual({
			source: 'jellyfin',
			track_id: 'jf-beacon',
			position_ms: 45000,
			start_position_ms: 0,
			duration_ms: 180000
		});
	});
});

describe('track failure reporting', () => {
	function resetFailures(): void {
		resetPlayer();
		vi.useFakeTimers();
		mockApiHead.mockResolvedValue(new Response(null, { status: 200 }));
	}

	beforeEach(resetFailures);

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

	function fireCurrentTrackError(): void {
		capturedErrorCallbacks.forEach((cb) => cb({ code: '4', message: 'stream failed' }));
	}

	function singleSourceItems(count: number): QueueItem[] {
		return Array.from({ length: count }, (_, i) => {
			const id = `vid-${i}`;
			return makeItem({
				trackSourceId: id,
				trackName: `Track ${i + 1}`,
				trackNumber: i + 1,
				availableSources: ['local'],
				sourceIds: { local: id }
			});
		});
	}

	it('retries a track once, earning the retry back only after 30 seconds heard', async () => {
		{
			// Retries the same track once, silently, before advancing.
			resetFailures();
			playerStore.playQueue(singleSourceItems(2));
			await vi.advanceTimersByTimeAsync(0);

			fireCurrentTrackError();
			await vi.advanceTimersByTimeAsync(0);

			// Retry reloads the same track instead of skipping onward.
			expect(playerStore.currentIndex).toBe(0);
			expect(toastStore.notify).not.toHaveBeenCalled();

			fireCurrentTrackError();
			await vi.advanceTimersByTimeAsync(2100);

			expect(playerStore.currentIndex).toBe(1);
			expect(toastStore.notify).toHaveBeenCalledTimes(1);
			expect(toastStore.notify).toHaveBeenCalledWith(
				'Skipped "Track 1": can\'t play it',
				'warning'
			);
		}

		{
			// Earns its retry back after 30 seconds heard, so a second drop still retries.
			resetFailures();
			playerStore.playQueue(singleSourceItems(2));
			await vi.advanceTimersByTimeAsync(0);

			for (let drop = 0; drop < 2; drop++) {
				playFor(30);
				fireCurrentTrackError();
				await vi.advanceTimersByTimeAsync(2100);
				expect(playerStore.currentIndex).toBe(0);
				expect(toastStore.notify).not.toHaveBeenCalled();
			}

			fireCurrentTrackError();
			await vi.advanceTimersByTimeAsync(2100);
			expect(playerStore.currentIndex).toBe(1);
		}

		{
			// Keeps spending its steps when it fails before 30 seconds or only seeks past them.
			resetFailures();
			playerStore.playQueue(singleSourceItems(2));
			await vi.advanceTimersByTimeAsync(0);

			playFor(29);
			fireCurrentTrackError();
			await vi.advanceTimersByTimeAsync(0);

			playFor(5);
			capturedProgressCallbacks.forEach((cb) => cb(500, 600));
			fireCurrentTrackError();
			await vi.advanceTimersByTimeAsync(2100);

			expect(playerStore.currentIndex).toBe(1);
			expect(toastStore.notify).toHaveBeenCalledTimes(1);
		}
	});

	it('picks a dropped track up where the listener was', async () => {
		playerStore.playQueue(singleSourceItems(2));
		await vi.advanceTimersByTimeAsync(0);
		capturedProgressCallbacks.forEach((cb) => cb(95, 600));

		fireCurrentTrackError();
		await vi.advanceTimersByTimeAsync(0);

		const reloaded = vi.mocked(createPlaybackSource).mock.results.at(-1)?.value;
		expect(playerStore.currentIndex).toBe(0);
		expect(reloaded.seekTo).toHaveBeenCalledWith(95);
		expect(playerStore.progress).toBe(95);
	});

	function playFor(seconds: number): void {
		for (let t = 1; t <= seconds; t++) {
			capturedProgressCallbacks.forEach((cb) => cb(t, 600));
		}
	}

	it('falls back to the next available source before skipping', async () => {
		const plexId = 'plex-part-key';
		const plexItem = makeItem({
			trackSourceId: plexId,
			trackName: 'Plex Track',
			sourceType: 'plex',
			availableSources: ['plex', 'local'],
			sourceIds: { plex: plexId, local: 'file-1' }
		});
		playerStore.playQueue([
			plexItem,
			makeItem({ trackSourceId: 'vid-next', trackName: 'Next Track' })
		]);
		await vi.advanceTimersByTimeAsync(0);

		fireCurrentTrackError();
		await vi.advanceTimersByTimeAsync(0);
		fireCurrentTrackError();
		await vi.advanceTimersByTimeAsync(0);

		// Same track now plays from the alternate source instead of skipping.
		expect(playerStore.currentIndex).toBe(0);
		expect(playerStore.queue[0].sourceType).toBe('local');
		expect(playerStore.queue[0].trackSourceId).toBe('file-1');
		expect(toastStore.notify).not.toHaveBeenCalled();

		fireCurrentTrackError();
		await vi.advanceTimersByTimeAsync(2100);

		expect(playerStore.currentIndex).toBe(1);
		expect(toastStore.notify).toHaveBeenCalledTimes(1);
		expect(toastStore.notify).toHaveBeenCalledWith(
			'Skipped "Plex Track": can\'t play it',
			'warning'
		);
	});
});
