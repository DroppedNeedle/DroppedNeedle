import type {
	PlaybackSource,
	PlaybackState,
	NowPlaying,
	QueueItem,
	SourceType
} from '$lib/player/types';
import { createPlaybackSource } from '$lib/player/createSource';
import { api } from '$lib/api/client';
import { createPlaybackSession, sessionKeyFor } from '$lib/player/playbackSession';
import { prefetchPathFor, streamUrlFor } from '$lib/player/streamUrl';
import { playbackToast } from '$lib/stores/playbackToast.svelte';
import { radioSession } from '$lib/stores/radioSession.svelte';
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
import { resumeAudioEngine } from '$lib/player/audioElement';
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

/**
 * How the current item is recovering. A failed item is retried once, then moved to another
 * source once, then counted failed and skipped after a pause; three failures in a row stop
 * playback. Playing clears it, so recovery never chains.
 */
type RecoveryPhase = 'idle' | 'loading' | 'playing' | 'retrying' | 'fallingBack' | 'failed';
type LoadKind = 'fresh' | 'retry' | 'fallback';

interface Recovery {
	phase: RecoveryPhase;
	/** Bumped by every load: a callback from an older load is ignored. */
	generation: number;
	/** The tracks that failed in a row; its length is the error count. */
	failed: string[];
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
	const recovery: Recovery = { phase: 'idle', generation: 0, failed: [], skipTimer: null };
	let lastPersistTime = 0;

	function cancelSkip(): void {
		if (recovery.skipTimer) clearTimeout(recovery.skipTimer);
		recovery.skipTimer = null;
	}

	/** Starts a load in the given phase and returns its generation. */
	function beginLoad(phase: RecoveryPhase): number {
		cancelSkip();
		recovery.phase = phase;
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
		position: () => ({ seconds: progress, isPaused: playbackState !== 'playing' })
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
		const url = streamUrlFor(item);
		if (item.sourceType === 'youtube') {
			return { source: createPlaybackSource('youtube'), loadUrl: url };
		}
		return {
			source: createPlaybackSource(item.sourceType, { url: url!, seekable: true }),
			loadUrl: url
		};
	}

	async function loadQueueItem(index: number, kind: LoadKind = 'fresh'): Promise<void> {
		const item = queue[index];
		if (!item) return;
		const gen = beginLoad(LOAD_PHASE[kind]);
		const prevProgress = progress,
			prevItem = currentSource ? (queue[currentIndex] ?? null) : null;
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
			const r = await resolveSourceForItem(item);
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
		if (!isCurrent(gen)) return;
		const index = currentIndex;
		const item = queue[index];
		const trackName = nowPlaying?.trackName ?? item?.trackName ?? 'Unknown track';
		const failedSource = item?.sourceType ?? nowPlaying?.sourceType ?? 'unknown';
		const detail = error?.message || error?.code || '';
		const detailSuffix = detail ? ` (${detail})` : '';
		const { phase } = recovery;
		// A first failure may be transient: reload the same item once.
		if (item && (phase === 'loading' || phase === 'playing')) {
			playbackToast.show(
				`"${trackName}" failed on ${failedSource}${detailSuffix}, retrying...`,
				'warning'
			);
			void loadQueueItem(index, 'retry');
			return;
		}
		// The same track may play from another source (Plex fails, the local copy works).
		if (item && phase === 'retrying') {
			const next = nextFallbackSource(item);
			const switched = next ? changeItemSource(queue, index, next) : null;
			if (switched && !switched.error) {
				queue = switched.newQueue;
				playbackToast.show(
					`"${trackName}" failed on ${failedSource}, trying ${next}...`,
					'warning'
				);
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
			playbackToast.show(`Several tracks failed: ${named} - playback stopped.`, 'error');
			applyResetState();
			return;
		}
		const nextIdx = getNextIndex();
		if (nextIdx !== null) {
			playbackToast.show(
				`"${trackName}" failed on ${failedSource}${detailSuffix}, skipping...`,
				'warning'
			);
			recovery.skipTimer = setTimeout(() => {
				recovery.skipTimer = null;
				if (isCurrent(gen)) void loadQueueItem(nextIdx);
			}, ERROR_SKIP_DELAY_MS);
		} else {
			playbackToast.show(`"${trackName}" failed on ${failedSource}${detailSuffix}`, 'error');
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
				recovery.phase = 'playing';
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
				playbackToast.show('Cannot change source for the currently playing track', 'warning');
				return;
			}
			const r = changeItemSource(queue, index, newSourceType);
			if (r.error) {
				playbackToast.show(r.error, 'warning');
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
