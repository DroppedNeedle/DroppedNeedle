import type {
	PlaybackSource,
	PlaybackState,
	NowPlaying,
	QueueItem,
	SourceType
} from '$lib/features/player/playback/types';
import { createPlaybackSource } from '$lib/features/player/playback/createSource';
import { api } from '$lib/api/client';
import {
	createPlaybackSession,
	sessionKeyFor
} from '$lib/features/player/playback/playbackSession';
import { prefetchPathFor, streamUrlFor } from '$lib/features/player/playback/streamUrl';
import { transcodeTarget } from '$lib/features/player/playback/transcodeTarget';
import { UNPLAYABLE_ERROR } from '$lib/features/player/playback/NativeAudioSource';
import { toastStore } from '$lib/stores/toast';
import { radioSession } from '$lib/features/player/state/radioSession.svelte';
import {
	getStoredVolume,
	storeVolume,
	storeSessionData,
	stampOrigin,
	stampSingleOrigin,
	showQueueMutationToast,
	type StoredSession
} from './playerUtils';
import {
	computeNextIndex,
	computePreviousIndex,
	computeUpcomingLength,
	computeNextAlbumIndex,
	computePreviousAlbumIndex,
	queueHasAlbums,
	performCleanup
} from './playerQueueOps';
import { buildNowPlayingMetadata } from './playerSourceResolver';
import { resumeAudioEngine } from '$lib/features/player/playback/audioElement';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';
import {
	persistSession as doPersistSession,
	restoreSessionData,
	buildResumeState
} from './playerSessionManager';
import {
	addItemToQueue,
	addMultipleItems,
	insertPlayNext,
	insertMultipleNext,
	removeAtIndex,
	performReorder,
	performShuffleReorder,
	clearQueueKeepCurrent
} from './playerQueueMethods';
import {
	buildPlayQueueState,
	computeToggleShuffle,
	changeItemSource,
	updateItemByPlaylistTrackId
} from './playerPlaybackMethods';

const MAX_CONSECUTIVE_ERRORS = 3;
const PREVIEW_FADE_S = 2;
const ERROR_SKIP_DELAY_MS = 2000;
const MAX_HISTORY_LENGTH = 3;
const SESSION_PERSIST_INTERVAL_MS = 5000;
const SESSION_REPORT_INTERVAL_MS = 10_000;
const MAX_SESSION_REPORT_FAILURES = 3;
const REARM_AFTER_PLAYED_S = 30;
// Progress arrives a few times a second; a bigger jump is a seek, not listening.
const MAX_PROGRESS_STEP_S = 5;

/**
 * How the current item is recovering. A failed item is reloaded once (as a transcode when the
 * browser could not read it, and not at all when it already was one), then moved to another
 * source once, without a word. Then it is counted failed and skipped after a pause, with one
 * message; three failures in a row stop playback. A track that fails soon after starting spends
 * the same steps, and only real playback clears the count. Thirty seconds heard since the last
 * load earns the steps back, so a dropped connection on a long track retries each time. A decode
 * error after that point is the file's, and is not retried: it would fail there again, for ever.
 */
type RecoveryPhase = 'idle' | 'loading' | 'retrying' | 'fallingBack' | 'failed';
type LoadKind = 'fresh' | 'retry' | 'fallback';

interface Recovery {
	phase: RecoveryPhase;
	/** Bumped by every load: a callback from an older load is ignored. */
	generation: number;
	/** The tracks that failed in a row; its length is the error count. */
	failed: string[];
	/** The current load asked the server for a transcode. */
	transcoded: boolean;
	/** The browser could not read this track's original, so its reloads stay transcodes. */
	directFailed: boolean;
	/** Seconds heard since the last load, counted from the player's own progress. */
	heard: number;
	lastPosition: number;
	skipTimer: ReturnType<typeof setTimeout> | null;
}

const LOAD_PHASE: Record<LoadKind, RecoveryPhase> = {
	fresh: 'loading',
	retry: 'retrying',
	fallback: 'fallingBack'
};

function createPlayerStore() {
	let currentSource = $state<PlaybackSource | null>(null);
	let nowPlaying = $state<NowPlaying | null>(null);
	let playbackState = $state<PlaybackState>('idle');
	let isSeekable = $state(true);
	let volume = $state(getStoredVolume());
	let progress = $state(0);
	let duration = $state(0);
	let isPlayerVisible = $state(false);
	let queue = $state<QueueItem[]>([]);
	let currentIndex = $state(0);
	let shuffleEnabled = $state(false);
	let shuffleOrder = $state<number[]>([]);
	const recovery: Recovery = {
		phase: 'idle',
		generation: 0,
		failed: [],
		transcoded: false,
		directFailed: false,
		heard: 0,
		lastPosition: 0,
		skipTimer: null
	};
	let lastPersistTime = 0;

	function cancelSkip(): void {
		if (recovery.skipTimer) clearTimeout(recovery.skipTimer);
		recovery.skipTimer = null;
	}

	/** Starts a load in the given phase and returns its generation. */
	function beginLoad(phase: RecoveryPhase): number {
		cancelSkip();
		recovery.phase = phase;
		recovery.heard = 0;
		recovery.lastPosition = 0;
		return ++recovery.generation;
	}

	function isCurrent(gen: number): boolean {
		return gen === recovery.generation;
	}

	const isPlaying = $derived(playbackState === 'playing');
	const isBuffering = $derived(playbackState === 'buffering' || playbackState === 'loading');
	const hasQueue = $derived(queue.length > 0);
	const hasNext = $derived.by(() => {
		if (queue.length <= 1) return false;
		if (shuffleEnabled) {
			const si = shuffleOrder.indexOf(currentIndex);
			return si < shuffleOrder.length - 1;
		}
		return currentIndex < queue.length - 1;
	});
	const hasPrevious = $derived.by(() => {
		if (queue.length <= 1) return false;
		if (shuffleEnabled) {
			const si = shuffleOrder.indexOf(currentIndex);
			return si > 0;
		}
		return currentIndex > 0;
	});
	const currentQueueItem = $derived(queue.length > 0 ? queue[currentIndex] : null);
	const queueLength = $derived(queue.length);
	// Album-skip is disabled under shuffle, which has no album boundaries.
	const hasAlbumsInQueue = $derived(queueHasAlbums(queue));
	const hasNextAlbum = $derived(
		!shuffleEnabled && hasAlbumsInQueue && computeNextAlbumIndex(queue, currentIndex) !== null
	);
	const hasPreviousAlbum = $derived(
		!shuffleEnabled && hasAlbumsInQueue && computePreviousAlbumIndex(queue, currentIndex) !== null
	);
	const currentTrackNumber = $derived(
		shuffleEnabled && shuffleOrder.length > 0
			? shuffleOrder.indexOf(currentIndex) + 1 || currentIndex + 1
			: currentIndex + 1
	);

	const session = createPlaybackSession({
		intervalMs: SESSION_REPORT_INTERVAL_MS,
		maxFailures: MAX_SESSION_REPORT_FAILURES,
		position: () => ({ seconds: progress, isPaused: playbackState !== 'playing' }),
		duration: () => duration
	});

	function getNextIndex(): number | null {
		return computeNextIndex(currentIndex, queue.length, shuffleEnabled, shuffleOrder);
	}
	function getPreviousIndex(): number | null {
		return computePreviousIndex(currentIndex, queue.length, shuffleEnabled, shuffleOrder);
	}
	function persist(): void {
		doPersistSession(nowPlaying, queue, currentIndex, progress, shuffleEnabled, shuffleOrder);
	}

	function applyResetState(): void {
		radioSession.end();
		currentSource?.destroy();
		currentSource = null;
		nowPlaying = null;
		playbackState = 'idle';
		isSeekable = true;
		isPlayerVisible = false;
		progress = 0;
		duration = 0;
		queue = [];
		currentIndex = 0;
		shuffleOrder = [];
		shuffleEnabled = false;
		cancelSkip();
		recovery.phase = 'idle';
		recovery.failed = [];
		session.clear();
		storeSessionData(null);
	}

	function resolveSourceForItem(item: QueueItem): {
		source: PlaybackSource;
		loadUrl: string | undefined;
	} {
		isSeekable = true;
		// Only library files transcode: the gateway passes a media server's bytes through.
		const format =
			item.sourceType === 'local' ? transcodeTarget(item.format, recovery.directFailed) : null;
		recovery.transcoded = format !== null;
		const url = streamUrlFor(item, format ? { format } : undefined);
		if (item.sourceType === 'youtube') {
			return { source: createPlaybackSource('youtube'), loadUrl: url };
		}
		return {
			source: createPlaybackSource(item.sourceType, {
				url: url!,
				seekable: true,
				transcode: format
					? {
							duration: item.duration ?? 0,
							urlAt: (seconds) =>
								streamUrlFor(
									{ sourceType: 'local', trackSourceId: item.trackSourceId },
									{ format, start_seconds: seconds }
								)
						}
					: undefined
			}),
			loadUrl: url
		};
	}

	async function loadQueueItem(index: number, kind: LoadKind = 'fresh'): Promise<void> {
		const item = queue[index];
		if (!item) return;
		const gen = beginLoad(LOAD_PHASE[kind]);
		if (kind !== 'retry') recovery.directFailed = false;
		const prevProgress = progress,
			prevItem = currentSource ? (queue[currentIndex] ?? null) : null;
		// A reload of the same track picks up where the listener was.
		const resumeAt = kind === 'fresh' ? 0 : prevProgress;
		currentIndex = index;
		playbackState = 'loading';
		progress = 0;
		duration = 0;
		if (prevItem) {
			await session.end(prevProgress);
		} else {
			session.clear();
		}
		if (!isCurrent(gen)) return;
		currentSource?.destroy();
		let source: PlaybackSource, resolvedUrl: string | undefined;
		try {
			const r = resolveSourceForItem(item);
			source = r.source;
			resolvedUrl = r.loadUrl;
		} catch (e) {
			handleTrackError(
				gen,
				e instanceof Error ? { code: 'LOAD_ERROR', message: e.message } : undefined
			);
			return;
		}
		currentSource = source;
		nowPlaying = buildNowPlayingMetadata(queue[index] ?? item);
		persist();
		subscribeToSource(source, gen);
		source.setVolume(volume);
		try {
			const activeItem = queue[index] ?? item;
			const loadPromise = source.load({
				trackSourceId: activeItem.trackSourceId,
				url: resolvedUrl,
				format: activeItem.format
			});
			void session.begin(sessionKeyFor(activeItem));
			if (isCurrent(gen) && activeItem.sourceType !== 'youtube') {
				source.play();
			}
			await loadPromise;
			if (isCurrent(gen) && resumeAt > 0) {
				source.seekTo(resumeAt);
				progress = resumeAt;
			}
			if (isCurrent(gen) && activeItem.sourceType === 'youtube') {
				source.play();
			}
		} catch (e) {
			handleTrackError(
				gen,
				e instanceof Error ? { code: 'LOAD_ERROR', message: e.message } : undefined
			);
		}
	}

	function nextFallbackSource(item: QueueItem): SourceType | null {
		const alternates = (item.availableSources ?? []).filter(
			(s) => s !== item.sourceType && item.sourceIds?.[s]
		);
		return alternates[0] ?? null;
	}

	function handleTrackError(gen: number, error?: { code: string; message: string }): void {
		// A failed load reports twice, through the source and through its promise.
		if (!isCurrent(gen) || recovery.phase === 'failed') return;
		const index = currentIndex;
		const item = queue[index];
		const trackName = nowPlaying?.trackName ?? item?.trackName ?? 'Unknown track';
		const { phase, transcoded } = recovery;
		const unplayable = error?.code === UNPLAYABLE_ERROR;
		const fileBroke = unplayable && recovery.heard >= REARM_AFTER_PLAYED_S;
		// A first failure may be transient: reload once. A transcode that failed is
		// not one to repeat, and an original the browser could not read becomes one.
		if (item && phase === 'loading' && !transcoded && !fileBroke) {
			recovery.directFailed ||= unplayable;
			void loadQueueItem(index, 'retry');
			return;
		}
		// The same track may play from another source (Plex fails, the local copy works).
		if (item && (phase === 'retrying' || phase === 'loading')) {
			const next = nextFallbackSource(item);
			const switched = next ? changeItemSource(queue, index, next) : null;
			if (switched && !switched.error) {
				queue = switched.newQueue;
				void loadQueueItem(index, 'fallback');
				return;
			}
		}
		cancelSkip();
		recovery.phase = 'failed';
		recovery.failed.push(trackName);
		playbackState = 'error';
		if (recovery.failed.length >= MAX_CONSECUTIVE_ERRORS) {
			const named = recovery.failed.map((n) => `"${n}"`).join(', ');
			toastStore.notify(`Stopped: can't play ${named}`, 'error');
			applyResetState();
			return;
		}
		const nextIdx = getNextIndex();
		if (nextIdx !== null) {
			toastStore.notify(`Skipped "${trackName}": can't play it`, 'warning');
			recovery.skipTimer = setTimeout(() => {
				recovery.skipTimer = null;
				if (isCurrent(gen)) void loadQueueItem(nextIdx);
			}, ERROR_SKIP_DELAY_MS);
		} else {
			toastStore.notify(`Can't play "${trackName}"`, 'error');
		}
	}

	function noteHeard(position: number): void {
		const step = position - recovery.lastPosition;
		recovery.lastPosition = position;
		if (step <= 0 || step > MAX_PROGRESS_STEP_S) return;
		recovery.heard += step;
		if (recovery.heard >= REARM_AFTER_PLAYED_S && recovery.phase !== 'failed') {
			recovery.phase = 'loading';
			recovery.transcoded = false;
		}
	}

	function prefetchNext(): void {
		const nextIdx = getNextIndex();
		if (nextIdx === null) return;
		const nextItem = queue[nextIdx];
		if (!nextItem) return;
		const url = prefetchPathFor(nextItem);
		if (url) void api.head(url).catch(() => {});
	}

	function subscribeToSource(source: PlaybackSource, gen: number): void {
		source.onStateChange((state) => {
			if (!isCurrent(gen)) return;
			playbackState = state;
			if (state === 'playing') {
				recovery.failed = [];
				session.playing();
				prefetchNext();
			}
			if (state === 'paused') session.paused(progress);
			if (state === 'ended') {
				void session.end(progress);
				const nextIdx = getNextIndex();
				if (nextIdx !== null) {
					void loadQueueItem(nextIdx).then(() => {
						const c = performCleanup(
							queue,
							currentIndex,
							shuffleEnabled,
							shuffleOrder,
							MAX_HISTORY_LENGTH
						);
						queue = c.newQueue;
						currentIndex = c.newIndex;
						shuffleOrder = c.newShuffleOrder;
						persist();
					});
				} else {
					applyResetState();
				}
			}
		});
		source.onProgress((t, d) => {
			if (!isCurrent(gen)) return;
			progress = t;
			duration = d;
			noteHeard(t);
			// preview tier: DJ-style fade over the last 2s so 30s clips blend
			// instead of stopping dead
			const item = queue[currentIndex];
			if (item?.isPreview && d > 0) {
				const remaining = d - t;
				if (remaining <= PREVIEW_FADE_S && remaining >= 0) {
					source.setVolume(volume * Math.max(0, remaining / PREVIEW_FADE_S));
				}
			}
			const now = Date.now();
			if (now - lastPersistTime >= SESSION_PERSIST_INTERVAL_MS) {
				lastPersistTime = now;
				persist();
			}
		});
		source.onError((error) => {
			if (!isCurrent(gen)) return;
			handleTrackError(gen, error);
		});
	}

	return {
		get currentSource() {
			return currentSource;
		},
		get nowPlaying() {
			return nowPlaying;
		},
		get playbackState() {
			return playbackState;
		},
		get isPlaying() {
			return isPlaying;
		},
		get isBuffering() {
			return isBuffering;
		},
		get isSeekable() {
			return isSeekable;
		},
		get volume() {
			return volume;
		},
		get progress() {
			return progress;
		},
		get duration() {
			return duration;
		},
		get isPlayerVisible() {
			return isPlayerVisible;
		},
		get hasQueue() {
			return hasQueue;
		},
		get hasNext() {
			return hasNext;
		},
		get hasPrevious() {
			return hasPrevious;
		},
		get hasNextAlbum() {
			return hasNextAlbum;
		},
		get hasPreviousAlbum() {
			return hasPreviousAlbum;
		},
		get shuffleEnabled() {
			return shuffleEnabled;
		},
		get queue() {
			return queue;
		},
		get currentIndex() {
			return currentIndex;
		},
		get currentQueueItem() {
			return currentQueueItem;
		},
		get queueLength() {
			return queueLength;
		},
		get upcomingQueueLength() {
			return computeUpcomingLength(queue.length, currentIndex, shuffleEnabled, shuffleOrder);
		},
		get currentTrackNumber() {
			return currentTrackNumber;
		},
		get shuffleOrder() {
			return shuffleOrder;
		},

		playAlbum(source: PlaybackSource, metadata: NowPlaying): void {
			radioSession.end();
			void resumeAudioEngine();
			void session.end(progress);
			currentSource?.destroy();
			const gen = beginLoad('loading');
			recovery.failed = [];
			currentSource = source;
			nowPlaying = metadata;
			playbackState = 'loading';
			isSeekable = true;
			isPlayerVisible = true;
			queue = [];
			currentIndex = 0;
			shuffleOrder = [];
			subscribeToSource(source, gen);
			source.setVolume(volume);
			persist();
		},

		playQueue(items: QueueItem[], startIndex: number = 0, shuffle: boolean = false): void {
			if (items.length === 0) return;
			if (!items.some((item) => item.playlistTrackId?.startsWith('radio:'))) {
				radioSession.end();
			}
			void resumeAudioEngine();
			const s = buildPlayQueueState(items, startIndex, shuffle);
			queue = s.queue;
			shuffleEnabled = s.shuffleEnabled;
			shuffleOrder = s.shuffleOrder;
			isPlayerVisible = s.isPlayerVisible;
			recovery.failed = [];
			void loadQueueItem(s.startIndex);
		},

		nextTrack(): void {
			const idx = getNextIndex();
			if (idx !== null)
				void loadQueueItem(idx).then(() => {
					const c = performCleanup(
						queue,
						currentIndex,
						shuffleEnabled,
						shuffleOrder,
						MAX_HISTORY_LENGTH
					);
					queue = c.newQueue;
					currentIndex = c.newIndex;
					shuffleOrder = c.newShuffleOrder;
					persist();
				});
		},

		previousTrack(): void {
			const idx = getPreviousIndex();
			if (idx !== null) void loadQueueItem(idx);
		},

		nextAlbum(): void {
			if (!hasNextAlbum) return;
			const idx = computeNextAlbumIndex(queue, currentIndex);
			if (idx !== null)
				void loadQueueItem(idx).then(() => {
					const c = performCleanup(
						queue,
						currentIndex,
						shuffleEnabled,
						shuffleOrder,
						MAX_HISTORY_LENGTH
					);
					queue = c.newQueue;
					currentIndex = c.newIndex;
					shuffleOrder = c.newShuffleOrder;
					persist();
				});
		},

		previousAlbum(): void {
			if (!hasPreviousAlbum) return;
			const idx = computePreviousAlbumIndex(queue, currentIndex);
			if (idx !== null) void loadQueueItem(idx);
		},

		toggleShuffle(): void {
			const r = computeToggleShuffle(queue.length, currentIndex, shuffleEnabled);
			shuffleEnabled = r.shuffleEnabled;
			shuffleOrder = r.shuffleOrder;
		},

		jumpToTrack(index: number): void {
			if (index >= 0 && index < queue.length) void loadQueueItem(index);
		},

		addToQueue(item: QueueItem): void {
			if (queue.length === 0) {
				this.playQueue([stampSingleOrigin(item, 'manual')], 0, false);
				showQueueMutationToast('queue', 1);
				return;
			}
			const r = addItemToQueue(queue, item, shuffleEnabled, shuffleOrder);
			queue = r.newQueue;
			shuffleOrder = r.newShuffleOrder;
			persist();
			showQueueMutationToast('queue', 1);
		},

		addMultipleToQueue(items: QueueItem[]): void {
			if (items.length === 0) return;
			if (queue.length === 0) {
				this.playQueue(stampOrigin(items, 'manual'), 0, false);
				showQueueMutationToast('queue', items.length);
				return;
			}
			const r = addMultipleItems(queue, items, shuffleEnabled, shuffleOrder);
			queue = r.newQueue;
			shuffleOrder = r.newShuffleOrder;
			persist();
			showQueueMutationToast('queue', items.length);
		},

		appendQueueSilent(items: QueueItem[]): void {
			if (items.length === 0) return;
			const r = addMultipleItems(queue, items, shuffleEnabled, shuffleOrder);
			queue = r.newQueue;
			shuffleOrder = r.newShuffleOrder;
			persist();
		},

		regenerateShuffleOrder(): void {
			if (!shuffleEnabled || queue.length === 0) return;

			// Find current position in shuffle order (not raw queue index)
			const currentShufflePos = shuffleOrder.indexOf(currentIndex);

			// Keep everything up to and including current position (played + now playing)
			const kept =
				currentShufflePos >= 0 ? shuffleOrder.slice(0, currentShufflePos + 1) : [currentIndex];
			const keptSet = new Set(kept);

			// Everything else (including newly-appended indices) gets reshuffled
			const remaining = Array.from({ length: queue.length }, (_, i) => i).filter(
				(i) => !keptSet.has(i)
			);
			for (let i = remaining.length - 1; i > 0; i--) {
				const j = Math.floor(Math.random() * (i + 1));
				[remaining[i], remaining[j]] = [remaining[j], remaining[i]];
			}

			shuffleOrder = [...kept, ...remaining];
			persist();
		},

		playNext(item: QueueItem): void {
			if (queue.length === 0) {
				this.playQueue([stampSingleOrigin(item, 'manual')], 0, false);
				showQueueMutationToast('next', 1);
				return;
			}
			const r = insertPlayNext(queue, item, currentIndex, shuffleEnabled, shuffleOrder);
			queue = r.newQueue;
			shuffleOrder = r.newShuffleOrder;
			persist();
			showQueueMutationToast('next', 1);
		},

		playMultipleNext(items: QueueItem[]): void {
			if (items.length === 0) return;
			if (queue.length === 0) {
				this.playQueue(stampOrigin(items, 'manual'), 0, false);
				showQueueMutationToast('next', items.length);
				return;
			}
			const r = insertMultipleNext(queue, items, currentIndex, shuffleEnabled, shuffleOrder);
			queue = r.newQueue;
			shuffleOrder = r.newShuffleOrder;
			persist();
			showQueueMutationToast('next', items.length);
		},

		/** Takes library tracks a removal took out of the library out of the queue. */
		dropTracks(trackIds: readonly string[]): void {
			const gone = new Set(trackIds);
			const isGone = (item: QueueItem | undefined) =>
				item?.sourceType === 'local' && gone.has(item.trackSourceId);
			// From the end, so every index still names its item; the current one goes last.
			for (let index = queue.length - 1; index >= 0; index--) {
				if (index === currentIndex || !isGone(queue[index])) continue;
				const r = removeAtIndex(queue, index, currentIndex, shuffleEnabled, shuffleOrder);
				queue = r.newQueue;
				currentIndex = r.newIndex;
				shuffleOrder = r.newShuffleOrder;
			}
			if (isGone(queue[currentIndex])) {
				this.removeFromQueue(currentIndex);
			} else {
				persist();
			}
		},

		removeFromQueue(index: number): void {
			if (index < 0 || index >= queue.length) return;
			if (queue.length <= 1) {
				this.stop();
				return;
			}
			const r = removeAtIndex(queue, index, currentIndex, shuffleEnabled, shuffleOrder);
			queue = r.newQueue;
			currentIndex = r.newIndex;
			shuffleOrder = r.newShuffleOrder;
			if (r.wasPlaying) {
				void loadQueueItem(r.newIndex);
			} else {
				persist();
			}
		},

		reorderQueue(fromIndex: number, toIndex: number): void {
			const r = performReorder(queue, fromIndex, toIndex, currentIndex);
			queue = r.newQueue;
			currentIndex = r.newCurrentIndex;
			persist();
		},

		reorderShuffleOrder(fromPos: number, toPos: number): void {
			shuffleOrder = performShuffleReorder(shuffleOrder, fromPos, toPos);
			persist();
		},

		clearQueue(): void {
			radioSession.end();
			if (queue.length === 0 || !queue[currentIndex]) {
				this.stop();
				return;
			}
			const r = clearQueueKeepCurrent(queue, currentIndex);
			queue = r.newQueue;
			currentIndex = r.newIndex;
			shuffleEnabled = false;
			shuffleOrder = [];
			persist();
		},

		changeTrackSource(index: number, newSourceType: SourceType): void {
			if (index < 0 || index >= queue.length) return;
			if (index === currentIndex) {
				toastStore.notify('Cannot change source for the currently playing track', 'warning');
				return;
			}
			const r = changeItemSource(queue, index, newSourceType);
			if (r.error) {
				toastStore.notify(r.error, 'warning');
				return;
			}
			queue = r.newQueue;
			persist();
		},

		/** Radio hydration: patch a not-yet-playing item (video id / preview URL)
		 * without touching the current track. */
		patchQueueItemByPlaylistTrackId(playlistTrackId: string, patch: Partial<QueueItem>): boolean {
			const index = queue.findIndex((q) => q.playlistTrackId === playlistTrackId);
			if (index < 0 || index === currentIndex) return false;
			const uq = [...queue];
			uq[index] = { ...uq[index], ...patch };
			queue = uq;
			persist();
			return true;
		},

		updateQueueItemByPlaylistTrackId(
			playlistTrackId: string,
			newSourceType: SourceType,
			newTrackSourceId: string,
			newFormat?: string,
			plexRatingKey?: string
		): void {
			const r = updateItemByPlaylistTrackId(
				queue,
				playlistTrackId,
				currentIndex,
				newSourceType,
				newTrackSourceId,
				newFormat,
				plexRatingKey
			);
			if (r) {
				queue = r;
				persist();
			}
		},

		play(): void {
			void resumeAudioEngine();
			currentSource?.play();
		},

		pause(): void {
			currentSource?.pause();
			session.paused(progress);
			persist();
		},

		togglePlay(): void {
			if (isPlaying) {
				currentSource?.pause();
				session.paused(progress);
				persist();
			} else {
				void resumeAudioEngine();
				currentSource?.play();
			}
		},
		seekTo(seconds: number): void {
			currentSource?.seekTo(seconds);
			progress = seconds;
			persist();
		},

		setVolume(level: number): void {
			const clamped = Math.max(0, Math.min(100, level));
			volume = clamped;
			currentSource?.setVolume(clamped);
			storeVolume(clamped);
		},

		stop(): void {
			void session.end(progress);
			recovery.generation++;
			applyResetState();
		},

		restoreSession(): StoredSession | null {
			return restoreSessionData();
		},

		resumeSession(): void {
			const stored = restoreSessionData();
			if (!stored) return;
			const resume = buildResumeState(stored);
			if (!resume) return;

			queue = resume.queue;
			shuffleEnabled = resume.shuffleEnabled;
			shuffleOrder = resume.shuffleOrder;
			isPlayerVisible = true;
			recovery.failed = [];
			void session.end(progress);
			currentSource?.destroy();
			currentIndex = resume.currentIndex;
			playbackState = 'loading';
			isSeekable = true;
			progress = 0;
			duration = 0;
			const gen = beginLoad('loading');

			void (async () => {
				try {
					const { source, loadUrl } = resolveSourceForItem(resume.currentItem);
					if (!isCurrent(gen)) return;
					currentSource = source;
					nowPlaying = resume.nowPlaying;
					subscribeToSource(source, gen);
					source.setVolume(volume);
					void session.begin(sessionKeyFor(resume.currentItem));
					await source.load({
						trackSourceId: resume.currentItem.trackSourceId,
						url: loadUrl,
						format: resume.currentItem.format
					});
					if (!isCurrent(gen)) return;
					playbackState = 'paused';
					duration = source.getDuration();
					if (resume.progress > 0) {
						source.seekTo(resume.progress);
						progress = resume.progress;
					}
				} catch {
					if (!isCurrent(gen)) return;
					playbackState = 'error';
					storeSessionData(null);
				}
			})();
		}
	};
}

export const playerStore = createPlayerStore();

// The next person on this browser never resumes the last one's listening.
registerUserSessionReset(() => playerStore.stop());
