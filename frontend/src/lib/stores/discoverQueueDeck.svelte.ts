/**
 * State machine for the always-visible Discover Queue deck.
 *
 * Load order favours instant paint after source validation: the saved deck,
 * then lightweight server candidates. Only the current and next card hydrate.
 * The queue is consumed-once and changed locally (advance/ignore/jump); every
 * change saves the deck through the query cache so a reload resumes it.
 */
import { isAbortError } from '$lib/utils/errorHandling';
import { applyEffects, recommendationsChanged } from '$lib/queries/effects';
import { authStore } from '$lib/stores/authStore.svelte';
import { recordDiscoverActivity } from '$lib/queries/discover/DiscoverDemand.svelte';
import {
	fetchDiscoverQueue,
	fetchDiscoverQueueStatus,
	fetchQueueEnrichment,
	fetchQueueInLibrary,
	forgetSavedDeck,
	generateDiscoverQueue,
	ignoreQueueItem,
	markDiscoverQueueConsumed,
	peekSavedDeck,
	readSavedDeck,
	saveDeck,
	watchDiscoverQueueStatus,
	type QueueStatus
} from '$lib/queries/discover/DiscoverQueueQueries';
import { discoverQueueAutoGenerate } from '$lib/queries/AdvancedSettingsReads.svelte';
import {
	musicBrainzSourceStamp,
	subscribeMusicBrainzSourceScope,
	watchMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import type { DeckCard, QueueEnrichment } from '$lib/queries/discover/types';

export type DeckPhase = 'idle' | 'loading' | 'building' | 'ready' | 'finished' | 'empty' | 'error';

function emptyEnrichment(): QueueEnrichment {
	return {
		artist_mbid: null,
		release_date: null,
		country: null,
		tags: [],
		youtube_url: null,
		youtube_search_url: '',
		youtube_search_available: false,
		artist_description: null,
		listen_count: null
	};
}

function dedupeByMbid(items: DeckCard[]): DeckCard[] {
	const seen = new SvelteSet<string>();
	const unique: DeckCard[] = [];
	for (const item of items) {
		const mbid = item.musicbrainz_id;
		if (!mbid || seen.has(mbid)) continue;
		seen.add(mbid);
		unique.push(item);
	}
	return unique;
}

function createDiscoverQueueDeck() {
	let phase = $state<DeckPhase>('idle');
	let queue = $state<DeckCard[]>([]);
	let currentIndex = $state(0);
	let queueId = $state('');
	let errorMessage = $state('');
	let replacing = $state(false);
	let generation = $state(0);
	let sourceUnsub: (() => void) | null = null;
	let storageUnsub: (() => void) | null = null;
	let isVisible = () => true;

	function requestKey(): string {
		return `${generation}:${musicBrainzSourceStamp()}`;
	}

	let abortController: AbortController | null = null;
	let statusUnsub: (() => void) | null = null;
	const inFlightEnrich = new SvelteMap<string, Promise<QueueEnrichment | null>>();

	const current = $derived(queue[currentIndex]);
	const isLast = $derived(currentIndex >= queue.length - 1);

	function persist(): void {
		void saveDeck({
			items: queue.map((item) => ({ ...item })),
			currentIndex,
			queueId
		});
	}

	function stopWatchingStatus(): void {
		if (statusUnsub) {
			statusUnsub();
			statusUnsub = null;
		}
	}

	/** A build settled: adopt the fresh queue, or show why it failed. */
	function onBuildStatus(s: QueueStatus): void {
		if (phase !== 'building' && phase !== 'finished' && !replacing) return;
		if (s.status === 'ready') {
			void fetchQueue();
		} else if (s.status === 'error') {
			replacing = false;
			phase = queue.length ? 'ready' : 'error';
			errorMessage = s.error ?? 'Queue build failed';
			stopWatchingStatus();
		}
	}

	/**
	 * When a background build finishes, adopt the fresh queue automatically.
	 * Only statuses after this call count: a stale 'ready' left from a prior
	 * build would otherwise adopt the old queue before the new build starts.
	 * init() handles a genuine already-ready status itself.
	 */
	function watchStatusUntilReady(): void {
		stopWatchingStatus();
		statusUnsub = watchDiscoverQueueStatus(onBuildStatus);
	}

	function startBuild(force: boolean): void {
		watchStatusUntilReady();
		void generateDiscoverQueue(force).then(onBuildStatus);
	}

	async function fetchQueue(): Promise<void> {
		stopWatchingStatus();
		const key = requestKey();
		if (!queue.length) phase = 'loading';
		try {
			const data = await fetchDiscoverQueue();
			if (key !== requestKey() || abortController?.signal.aborted) return;
			generation++;
			replacing = false;
			queue = dedupeByMbid(data.items ?? []);
			queueId = data.queue_id;
			currentIndex = 0;
			inFlightEnrich.clear();
			if (queue.length === 0) {
				phase = 'empty';
				return;
			}
			phase = 'ready';
			persist();
			void enrichWindow();
			void markDiscoverQueueConsumed();
		} catch (e) {
			if (isAbortError(e) || key !== requestKey()) return;
			replacing = false;
			phase = queue.length ? 'ready' : 'error';
			errorMessage = 'Could not load your discovery queue';
		}
	}

	async function validateCachedQueue(): Promise<void> {
		if (queue.length === 0) return;
		const key = requestKey();
		try {
			const mbids = queue.map((i) => i.musicbrainz_id);
			const data = await fetchQueueInLibrary(mbids);
			if (key !== requestKey()) return;
			const inLibrary = new SvelteSet(data.in_library || []);
			if (inLibrary.size > 0) {
				queue = queue.filter((i) => !inLibrary.has(i.musicbrainz_id));
				if (currentIndex >= queue.length) currentIndex = Math.max(0, queue.length - 1);
				persist();
			}
			if (queue.length === 0) {
				await fetchQueue();
			}
		} catch {
			// validation is best-effort; a stale in-library item is survivable
		}
	}

	async function enrichItem(index: number): Promise<void> {
		const item = queue[index];
		if (!item || item.enrichment) return;

		const mbid = item.musicbrainz_id;
		const key = requestKey();
		const existing = inFlightEnrich.get(mbid);
		if (existing) {
			await existing;
			return;
		}

		const promise = (async (): Promise<QueueEnrichment | null> => {
			try {
				const data = await fetchQueueEnrichment(mbid);
				if (key !== requestKey()) return null;
				const idx = queue.findIndex((q) => q.musicbrainz_id === mbid);
				if (idx >= 0 && !queue[idx].enrichment) {
					queue[idx] = { ...queue[idx], enrichment: data };
				}
				return data;
			} catch (e) {
				if (isAbortError(e) || key !== requestKey()) return null;
				const idx = queue.findIndex((q) => q.musicbrainz_id === mbid);
				if (idx >= 0 && !queue[idx].enrichment) {
					queue[idx] = { ...queue[idx], enrichment: emptyEnrichment() };
				}
				return null;
			} finally {
				if (key === requestKey()) inFlightEnrich.delete(mbid);
			}
		})();
		inFlightEnrich.set(mbid, promise);
		await promise;
	}

	async function enrichWindow(): Promise<void> {
		if (queue.length === 0) return;
		void enrichItem(currentIndex);
		void enrichItem(currentIndex + 1);
	}

	return {
		get requestKey() {
			return requestKey();
		},
		get replacing() {
			return replacing;
		},
		get phase() {
			return phase;
		},
		get queue() {
			return queue;
		},
		get currentIndex() {
			return currentIndex;
		},
		get current() {
			return current;
		},
		get isLast() {
			return isLast;
		},
		get errorMessage() {
			return errorMessage;
		},

		async init(getVisible?: () => boolean): Promise<void> {
			if (getVisible) isVisible = getVisible;
			generation++;
			const session = generation;
			sourceUnsub?.();
			storageUnsub?.();
			sourceUnsub = null;
			storageUnsub = null;
			queue = [];
			phase = 'loading';
			replacing = false;
			inFlightEnrich.clear();
			abortController?.abort();
			abortController = new AbortController();
			// clean up any watcher left over from a prior init without an
			// intervening destroy (HMR, double-mount) so we don't orphan a poll loop
			stopWatchingStatus();
			try {
				await recordDiscoverActivity({ feature: 'queue' }, abortController.signal);
			} catch (e) {
				if (session !== generation || isAbortError(e)) return;
				phase = 'error';
				errorMessage = 'Could not verify your discovery source. Retry to load your queue.';
				return;
			}
			if (session !== generation) return;
			errorMessage = '';
			// A source switch drops that source's saved deck from the query cache.
			sourceUnsub = subscribeMusicBrainzSourceScope(() => {
				if (isVisible()) void this.init();
				else this.destroy();
			});
			storageUnsub = watchMusicBrainzSourceScope();

			const cached = await readSavedDeck();
			if (session !== generation) return;
			const cachedCurrentMbid = cached
				? cached.items[cached.currentIndex]?.musicbrainz_id
				: undefined;
			const cachedItems = cached ? dedupeByMbid(cached.items) : [];
			if (cached && cachedItems.length > 0) {
				queue = cachedItems;
				const resumedIndex = cachedCurrentMbid
					? queue.findIndex((item) => item.musicbrainz_id === cachedCurrentMbid)
					: -1;
				currentIndex =
					resumedIndex >= 0
						? resumedIndex
						: Math.max(0, Math.min(cached.currentIndex, queue.length - 1));
				queueId = cached.queueId;
				phase = 'ready';
				await validateCachedQueue();
				void enrichWindow();
				return;
			}

			phase = 'loading';
			const status = await fetchDiscoverQueueStatus();
			if (session !== generation) return;
			if (status?.status === 'ready') {
				await fetchQueue();
				return;
			}
			if (status?.status === 'building') {
				phase = 'building';
				watchStatusUntilReady();
				return;
			}
			if (status?.status === 'idle' && (await discoverQueueAutoGenerate())) {
				if (session !== generation) return;
				phase = 'building';
				watchStatusUntilReady();
				onBuildStatus(await generateDiscoverQueue(false));
				return;
			}
			// no background machinery available: build inline
			await fetchQueue();
		},

		next(): void {
			if (isLast) return;
			currentIndex++;
			void enrichWindow();
			persist();
		},

		previous(): void {
			if (currentIndex === 0) return;
			currentIndex--;
			void enrichWindow();
			persist();
		},

		jumpTo(index: number): void {
			if (index < 0 || index >= queue.length || index === currentIndex) return;
			currentIndex = index;
			void enrichWindow();
			persist();
		},

		removeByMbid(mbid: string): void {
			const removedIndex = queue.findIndex((item) => item.musicbrainz_id === mbid);
			if (removedIndex < 0) {
				const cached = peekSavedDeck();
				if (!cached) return;
				const items = cached.items.filter((item) => item.musicbrainz_id !== mbid);
				if (items.length === cached.items.length) return;
				if (items.length === 0) {
					void forgetSavedDeck();
					return;
				}
				void saveDeck({
					items,
					queueId: cached.queueId,
					currentIndex: Math.min(cached.currentIndex, items.length - 1)
				});
				return;
			}

			queue = queue.filter((item) => item.musicbrainz_id !== mbid);
			if (removedIndex < currentIndex) currentIndex--;
			if (currentIndex >= queue.length) currentIndex = Math.max(0, queue.length - 1);
			if (queue.length === 0) {
				this.finish();
				return;
			}
			void enrichWindow();
			persist();
		},

		async ignoreCurrent(): Promise<void> {
			const item = current;
			if (!item) return;
			const key = requestKey();
			let saved = false;
			try {
				await ignoreQueueItem({
					releaseGroupMbid: item.musicbrainz_id,
					artistMbid: item.artist_id,
					releaseName: item.title,
					artistName: item.artist_name
				});
				saved = true;
			} catch {
				// removing it locally is still right even if the ignore write failed
			}
			if (key !== requestKey()) return;
			this.removeByMbid(item.musicbrainz_id);
			if (saved) await applyEffects(recommendationsChanged(authStore.user?.id));
		},

		markCurrentRequested(): void {
			if (!current) return;
			queue[currentIndex] = { ...current, requested: true };
			persist();
		},

		/** End of the deck: clear the cache and brew a fresh queue in the background. */
		finish(): void {
			generation++;
			queue = [];
			currentIndex = 0;
			queueId = '';
			inFlightEnrich.clear();
			void forgetSavedDeck();
			phase = 'finished';
			const session = generation;
			void discoverQueueAutoGenerate().then((auto) => {
				if (auto && session === generation) startBuild(false);
			});
		},

		retryBuild(): void {
			if (!sourceUnsub) {
				void this.init();
				return;
			}
			this.buildNow();
		},

		buildNow(): void {
			if (!sourceUnsub) {
				void this.init().then(() => {
					if (sourceUnsub) this.buildNow();
				});
				return;
			}
			if (replacing) return;
			replacing = true;
			if (!queue.length) phase = 'building';
			errorMessage = '';
			startBuild(true);
		},

		destroy(): void {
			generation++;
			sourceUnsub?.();
			storageUnsub?.();
			sourceUnsub = null;
			storageUnsub = null;
			abortController?.abort();
			abortController = null;
			stopWatchingStatus();
			inFlightEnrich.clear();
			phase = 'idle';
		}
	};
}

export const discoverQueueDeck = createDiscoverQueueDeck();
