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
import { browserPlays, transcodeTarget } from '$lib/features/player/playback/transcodeTarget';
import { STREAM_REFUSED, UNPLAYABLE_ERROR } from '$lib/features/player/playback/NativeAudioSource';
import {
	REFUSAL_SAYS,
	askWhyRefused,
	isSaidRefusal
} from '$lib/features/player/playback/streamRefusal';
import {
	getPlaybackConversionQueryOptions,
	type Conversion
} from '$lib/features/player/queries/conversion';
import { queryClient } from '$lib/queries/QueryClient';
import { authStore } from '$lib/stores/authStore.svelte';
import { withBasePath } from '$lib/utils/basePath';
import { goto } from '$app/navigation';
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
	computeAllowedIndex,
	computeNextIndex,
	computePreviousIndex,
	computeUpcomingLength,
	upcomingIndexes,
	computeNextAlbumIndex,
	computePreviousAlbumIndex,
	queueHasAlbums,
	performCleanup
} from './playerQueueOps';
import { buildNowPlayingMetadata } from './playerSourceResolver';
import { libraryTrackId, skipByRating, skippedRating } from './skipByRating.svelte';
import {
	cachedTrackRatings,
	readTrackRatings,
	warmTrackRatings,
	type TrackRatings
} from '$lib/features/library/queries/ratings.svelte';
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
// How long a pick waits for the queue's ratings before it plays on without them.
const RATINGS_WAIT_MS = 5000;

/**
 * How the current item is recovering. A failed item is reloaded once (as a transcode when the
 * browser could not read it, and not at all when it already was one), then moved to another
 * source once, without a word, where an original the browser could not read is reloaded once
 * more as a transcode. Then it is counted failed and skipped after a pause, with one message; three
 * failures in a row stop playback. A track that fails soon after starting spends the same steps,
 * and only real playback clears the count. Thirty seconds heard since the last load earns the
 * steps back, so a dropped connection on a long track retries each time. A decode error after
 * that point is the file's, and is not retried: it would fail there again, for ever. A load
 * the server refused (the source asks it why) is reloaded as it was, never as a transcode, and
 * its message says why; a conversion it refused fails as one it is not making.
 *
 * A track the browser cannot play while the server is not converting is never asked for: it
 * fails at once, may still move to another source, and is marked with why.
 */
type RecoveryPhase = 'idle' | 'loading' | 'retrying' | 'fallingBack' | 'failed';
type LoadKind = 'fresh' | 'retry' | 'fallback';

interface Recovery {
	phase: RecoveryPhase;
	/** Bumped by every load: a callback from an older load is ignored. */
	generation: number;
	/** The tracks that failed in a row and why; its length is the error count. */
	failed: { track: string; why: string }[];
	/** The current load asked the server for a transcode. */
	transcoded: boolean;
	/** The browser could not read this track's original, so its reloads stay transcodes. */
	directFailed: boolean;
	/** The item moved to another source once already. */
	fellBack: boolean;
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

/** Why a track cannot play here: the note the player shows and the queue marks it with. */
export type UnplayableReason = 'converting_off' | 'cannot_convert' | 'conversion_failed';

// The load error for a track the server would not convert; its message is the Conversion.
const NOT_CONVERTING = 'NOT_CONVERTING';

const CONNECT_APPS_SETTINGS = '/settings?tab=connect-apps';

function withinTime<T>(promise: Promise<T>, ms: number): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const late = new Promise<never>((_, reject) => {
		timer = setTimeout(() => reject(new Error('timed out')), ms);
	});
	return Promise.race([promise, late]).finally(() => clearTimeout(timer));
}

function itemKey(item: QueueItem): string {
	return `${item.sourceType}:${item.trackSourceId}`;
}

/** The format as a person reads it: `ALAC`, or "this format" when nobody named it. */
export function formatLabel(format: string | undefined): string {
	return format ? format.toUpperCase() : 'this format';
}

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
	// Whether the server converts, as last read; null until read, or when it could not be.
	let conversion = $state<Conversion | null>(null);
	// Queue items that failed to play here, by source and id, with why.
	let unplayable = $state<Record<string, UnplayableReason>>({});
	// The current load is a conversion for this browser.
	let converting = $state(false);
	// Playback stopped at a track's end because the rating floor passes over the rest.
	let floorStopped = $state(false);
	// A failed ratings read was told once; the next good read clears it.
	let ratingsReadFailed = false;
	const recovery: Recovery = {
		phase: 'idle',
		generation: 0,
		failed: [],
		transcoded: false,
		directFailed: false,
		fellBack: false,
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
		floorStopped = false;
		return ++recovery.generation;
	}

	function isCurrent(gen: number): boolean {
		return gen === recovery.generation;
	}

	const isPlaying = $derived(playbackState === 'playing');
	const isBuffering = $derived(playbackState === 'buffering' || playbackState === 'loading');
	const hasQueue = $derived(queue.length > 0);
	// The library tracks the rating floor judges; none while it is off.
	const floorTrackIds = $derived(
		skipByRating.on ? queue.map(libraryTrackId).filter((id): id is string => id !== null) : []
	);
	// What the cache holds of their ratings, followed as it changes.
	const cachedRatings = $derived(
		floorTrackIds.length > 0 ? cachedTrackRatings(floorTrackIds) : null
	);
	const hasNext = $derived(getNextIndex() !== null);
	const hasPrevious = $derived(getPreviousIndex() !== null);
	const upcomingLength = $derived.by(() => {
		if (cachedRatings === null) {
			return computeUpcomingLength(queue.length, currentIndex, shuffleEnabled, shuffleOrder);
		}
		return upcomingIndexes(queue.length, currentIndex, shuffleEnabled, shuffleOrder).filter(
			plays(cachedRatings)
		).length;
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

	$effect.root(() => {
		$effect(() => {
			// Reads only what no lookup holds, again after the cache lets one go.
			if (cachedRatings !== null) warmTrackRatings(floorTrackIds);
		});
	});

	/** Whether the queue item at an index plays under the floor, given these ratings. */
	function plays(ratings: TrackRatings | null): (index: number) => boolean {
		const floor = skipByRating.floor;
		return (index) => skippedRating(floor, ratings, queue[index]) === null;
	}

	// Next and previous pass over the tracks the rating floor skips, by the
	// ratings given, else by what the cache holds.
	function getNextIndex(ratings: TrackRatings | null = cachedRatings): number | null {
		return computeAllowedIndex(
			computeNextIndex,
			currentIndex,
			queue.length,
			shuffleEnabled,
			shuffleOrder,
			plays(ratings)
		);
	}
	function getPreviousIndex(ratings: TrackRatings | null = cachedRatings): number | null {
		return computeAllowedIndex(
			computePreviousIndex,
			currentIndex,
			queue.length,
			shuffleEnabled,
			shuffleOrder,
			plays(ratings)
		);
	}

	/**
	 * The queue's ratings for a pick, waiting for a read the cache lacks. A
	 * read that fails or is slow lets everything play, with one note.
	 */
	async function ratingsForPick(): Promise<TrackRatings | null> {
		if (floorTrackIds.length === 0) return null;
		try {
			const ratings = await withinTime(readTrackRatings(floorTrackIds), RATINGS_WAIT_MS);
			ratingsReadFailed = false;
			return ratings;
		} catch {
			if (!ratingsReadFailed) {
				toastStore.notify("Couldn't read your ratings, so nothing was skipped.", 'warning');
			}
			ratingsReadFailed = true;
			return null;
		}
	}

	interface Pick {
		index: number | null;
		/** Tracks are left that the floor passes over. */
		floorStopped: boolean;
	}

	/**
	 * Hands `use` the next (or previous) track to play: at once while the
	 * floor is off, else once the ratings are read, and never when another
	 * track loaded meanwhile.
	 */
	function pick(step: 'next' | 'previous', use: (picked: Pick) => void): void {
		const find = step === 'next' ? getNextIndex : getPreviousIndex;
		const any = find(null);
		if (any === null || floorTrackIds.length === 0) {
			use({ index: any, floorStopped: false });
			return;
		}
		const gen = recovery.generation;
		void ratingsForPick().then((ratings) => {
			if (!isCurrent(gen)) return;
			const index = find(ratings);
			use({ index, floorStopped: index === null });
		});
	}

	function loadAndTidy(index: number): void {
		void loadQueueItem(index).then(() => {
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
		unplayable = {};
		converting = false;
		floorStopped = false;
		session.clear();
		storeSessionData(null);
	}

	/** Reads whether the server converts, from the cache while it is fresh unless `fresh`. */
	async function readConversion(fresh = false): Promise<Conversion | null> {
		const options = getPlaybackConversionQueryOptions();
		try {
			const answer = await queryClient.fetchQuery(fresh ? { ...options, staleTime: 0 } : options);
			conversion = answer.conversion;
		} catch {
			conversion = null;
		}
		return conversion;
	}

	/** The browser cannot play this item's original, so it plays only as a conversion. */
	function needsConversion(item: QueueItem): boolean {
		return item.sourceType !== 'youtube' && !browserPlays(item.format);
	}

	/**
	 * Why this item cannot play here, or null: it failed already, or the browser
	 * cannot play its format, the server is not converting and no other source has it.
	 */
	function unplayableReasonFor(item: QueueItem): UnplayableReason | null {
		const known = unplayable[itemKey(item)];
		if (known) return known;
		if (!needsConversion(item) || nextFallbackSource(item)) return null;
		if (conversion === 'off') return 'converting_off';
		if (conversion === 'unavailable') return 'cannot_convert';
		return null;
	}

	function resolveSourceForItem(item: QueueItem): {
		source: PlaybackSource;
		loadUrl: string | undefined;
	} {
		isSeekable = true;
		const format =
			item.sourceType === 'youtube' ? null : transcodeTarget(item.format, recovery.directFailed);
		recovery.transcoded = format !== null;
		converting = format !== null;
		const url = streamUrlFor(item, format ? { format } : undefined);
		if (item.sourceType === 'youtube') {
			return { source: createPlaybackSource('youtube'), loadUrl: url };
		}
		const sourceType = item.sourceType;
		const source = createPlaybackSource(item.sourceType, {
			url: url!,
			seekable: true,
			whyRefused: async (failed) => {
				const why = await askWhyRefused(failed, sourceType !== 'local');
				// Converting stopped since it was read: whether it is off or cannot is read again.
				if (why === 'not_converting') await readConversion(true);
				return why;
			},
			transcode: format
				? {
						duration: item.duration ?? 0,
						urlAt: (seconds) =>
							streamUrlFor(
								{ sourceType, trackSourceId: item.trackSourceId },
								{ format, start_seconds: seconds }
							)
					}
				: undefined
		});
		isSeekable = source.isSeekable?.() ?? true;
		return { source, loadUrl: url };
	}

	async function loadQueueItem(index: number, kind: LoadKind = 'fresh'): Promise<void> {
		const item = queue[index];
		if (!item) return;
		const gen = beginLoad(LOAD_PHASE[kind]);
		if (kind !== 'retry') recovery.directFailed = false;
		if (kind === 'fresh') recovery.fellBack = false;
		if (kind === 'fallback') recovery.fellBack = true;
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
		if (recovery.directFailed || needsConversion(item)) {
			const state = await readConversion();
			if (!isCurrent(gen)) return;
			if (state === 'off' || state === 'unavailable') {
				currentSource?.destroy();
				currentSource = null;
				converting = false;
				nowPlaying = buildNowPlayingMetadata(item);
				handleTrackError(gen, { code: NOT_CONVERTING, message: state });
				return;
			}
		}
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

	function handleTrackError(gen: number, reported?: { code: string; message: string }): void {
		// A failed load reports twice, through the source and through its promise.
		if (!isCurrent(gen) || recovery.phase === 'failed') return;
		// A conversion the server refused fails as one it was found not making before the load.
		const error =
			reported?.code === STREAM_REFUSED && reported.message === 'not_converting'
				? { code: NOT_CONVERTING, message: conversion === 'off' ? 'off' : 'unavailable' }
				: reported;
		const index = currentIndex;
		const item = queue[index];
		const trackName = nowPlaying?.trackName ?? item?.trackName ?? 'Unknown track';
		const { phase, transcoded } = recovery;
		const undecodable = error?.code === UNPLAYABLE_ERROR;
		const notConverting = error?.code === NOT_CONVERTING;
		const refusal =
			error?.code === STREAM_REFUSED && isSaidRefusal(error.message) ? error.message : null;
		const fileBroke = undecodable && recovery.heard >= REARM_AFTER_PLAYED_S;
		// A first failure may be transient: reload once. A transcode that failed is
		// not one to repeat, and an original the browser could not read becomes one,
		// on the source it moved to as well.
		const reload = phase === 'loading' || (phase === 'fallingBack' && undecodable);
		if (item && reload && !transcoded && !fileBroke && !notConverting) {
			recovery.directFailed ||= undecodable;
			void loadQueueItem(index, 'retry');
			return;
		}
		// The same track may play from another source (Plex fails, the local copy works).
		if (item && !recovery.fellBack && (phase === 'retrying' || phase === 'loading')) {
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
		playbackState = 'error';
		// A refusal is the server's, not the track's, so the queue does not mark it.
		const reason: UnplayableReason | null = notConverting
			? error?.message === 'off'
				? 'converting_off'
				: 'cannot_convert'
			: transcoded && !refusal
				? 'conversion_failed'
				: null;
		if (item && reason) unplayable = { ...unplayable, [itemKey(item)]: reason };
		const why = notConverting
			? `this browser can't play ${formatLabel(item?.format)}`
			: refusal
				? REFUSAL_SAYS[refusal]
				: "can't play it";
		recovery.failed.push({ track: trackName, why });
		if (recovery.failed.length >= MAX_CONSECUTIVE_ERRORS) {
			const named = recovery.failed.map((failure) => `"${failure.track}"`).join(', ');
			const shared = recovery.failed.every((failure) => failure.why === why);
			toastStore.notify(
				!shared
					? "Stopped after three tracks in a row couldn't play"
					: refusal
						? `Stopped: ${why}`
						: `Stopped: can't play ${named}`,
				'error'
			);
			applyResetState();
			return;
		}
		// An admin can turn transcoding on; the note says where.
		const action =
			reason === 'converting_off' && authStore.isAdmin
				? {
						label: 'Turn on transcoding',
						run: () => goto(withBasePath(CONNECT_APPS_SETTINGS))
					}
				: undefined;
		const say = (message: string, type: 'warning' | 'error') =>
			action ? toastStore.show({ message, type, action }) : toastStore.notify(message, type);
		const nextIdx = getNextIndex();
		if (nextIdx !== null) {
			say(`Skipped "${trackName}": ${why}`, 'warning');
			recovery.skipTimer = setTimeout(() => {
				recovery.skipTimer = null;
				if (isCurrent(gen)) void loadQueueItem(nextIdx);
			}, ERROR_SKIP_DELAY_MS);
		} else {
			say(
				notConverting || refusal
					? `Can't play "${trackName}": ${why}`
					: `Can't play "${trackName}"`,
				'error'
			);
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
			recovery.fellBack = false;
		}
	}

	function prefetchNext(): void {
		const nextIdx = getNextIndex();
		if (nextIdx === null) return;
		const nextItem = queue[nextIdx];
		// A track that will not play here is not worth warming.
		if (!nextItem || unplayableReasonFor(nextItem)) return;
		const url = prefetchPathFor(nextItem);
		if (url) void api.head(url).catch(() => {});
	}

	function subscribeToSource(source: PlaybackSource, gen: number): void {
		source.onStateChange((state) => {
			if (!isCurrent(gen)) return;
			playbackState = state;
			if (state === 'playing') {
				floorStopped = false;
				recovery.failed = [];
				session.playing();
				prefetchNext();
			}
			if (state === 'paused') session.paused(progress);
			if (state === 'ended') {
				void session.end(progress);
				pick('next', (next) => {
					if (!isCurrent(gen)) return;
					if (next.index !== null) {
						loadAndTidy(next.index);
					} else if (next.floorStopped) {
						// The rest of the queue stays, for the floor to be lowered or a track picked.
						floorStopped = true;
					} else {
						applyResetState();
					}
				});
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
		/** The tracks still to play, less those the rating floor passes over. */
		get upcomingQueueLength() {
			return upcomingLength;
		},
		/** The library tracks the rating floor judges; empty while it is off. */
		get floorTrackIds() {
			return floorTrackIds;
		},
		/** What the cache holds of the queue's ratings while the floor is on, else null. */
		get floorRatings() {
			return cachedRatings;
		},
		/** Playback stopped because the rating floor passes over the rest of the queue. */
		get floorStopped() {
			return floorStopped;
		},
		get currentTrackNumber() {
			return currentTrackNumber;
		},
		get shuffleOrder() {
			return shuffleOrder;
		},
		/** The current track plays as a conversion for this browser. */
		get converting() {
			return converting;
		},

		unplayableReasonFor,

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
			unplayable = {};
			// So the queue can mark what will not play before it is reached.
			if (items.some(needsConversion)) void readConversion();
			void loadQueueItem(s.startIndex);
		},

		nextTrack(): void {
			pick('next', ({ index }) => {
				if (index !== null) loadAndTidy(index);
			});
		},

		previousTrack(): void {
			pick('previous', ({ index }) => {
				if (index !== null) void loadQueueItem(index);
			});
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
			if (!isSeekable) return;
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
