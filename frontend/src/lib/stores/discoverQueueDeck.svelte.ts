import { userStorageSegment } from '$lib/queries/userKeySegment';
/**
 * State machine for the always-visible Discover Queue deck.
 *
 * Load order favours instant paint after source validation: localStorage resume,
 * then lightweight server candidates. Only the current and next card hydrate.
 * The queue is consumed-once and mutated locally (advance/ignore/jump), so it lives
 * here rather than in TanStack Query; every mutation persists to localStorage.
 */
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { discoverQueueStatusStore } from '$lib/stores/discoverQueueStatus';
import { getCacheTTLs } from '$lib/stores/cacheTtl.svelte';
import {
	getQueueCachedData,
	removeQueueCachedData,
	setQueueCachedData
} from '$lib/utils/discoverQueueCache';
import { isAbortError } from '$lib/utils/errorHandling';
import { invalidateDiscoverRecommendations } from '$lib/queries/discover/DiscoverInvalidation';
import { recordDiscoverActivity } from '$lib/queries/discover/DiscoverDemand.svelte';
import { DiscoverV3Api } from '$lib/queries/discover/DiscoverV3Api';
import { toQueueEnrichment, toQueueItem } from '$lib/queries/discover/DiscoverV3Adapters';
import {
	musicBrainzSourceKey,
	subscribeMusicBrainzSourceScope,
	watchMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import { SvelteMap, SvelteSet } from 'svelte/reactivity';
import type { DiscoverQueueEnrichment, DiscoverQueueItemFull } from '$lib/types';

export type DeckPhase = 'idle' | 'loading' | 'building' | 'ready' | 'finished' | 'empty' | 'error';

function emptyEnrichment(): DiscoverQueueEnrichment {
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

function dedupeByMbid(items: DiscoverQueueItemFull[]): DiscoverQueueItemFull[] {
	const seen = new SvelteSet<string>();
	const unique: DiscoverQueueItemFull[] = [];
	for (const item of items) {
		const mbid = item.release_group_mbid;
		if (!mbid || seen.has(mbid)) continue;
		seen.add(mbid);
		unique.push(item);
	}
	return unique;
}

function createDiscoverQueueDeck() {
	let phase = $state<DeckPhase>('idle');
	let queue = $state<DiscoverQueueItemFull[]>([]);
	let currentIndex = $state(0);
	let queueId = $state('');
	let errorMessage = $state('');
	let replacing = $state(false);
	let generation = $state(0);
	let sourceUnsub: (() => void) | null = null;
	let storageUnsub: (() => void) | null = null;
	let isVisible = () => true;

	function requestKey(): string {
		return `${generation}:${JSON.stringify(musicBrainzSourceKey())}`;
	}

	let abortController: AbortController | null = null;
	let statusUnsub: (() => void) | null = null;
	const inFlightEnrich = new SvelteMap<string, Promise<DiscoverQueueEnrichment | null>>();

	const current = $derived(queue[currentIndex]);
	const isLast = $derived(currentIndex >= queue.length - 1);

	function userId(): string {
		return userStorageSegment(authStore.user?.id);
	}

	function persist(): void {
		setQueueCachedData(
			{
				items: queue.map((item) => ({ ...item })),
				currentIndex,
				queueId
			},
			userId()
		);
	}

	function stopWatchingStatus(): void {
		if (statusUnsub) {
			statusUnsub();
			statusUnsub = null;
		}
	}

	/** When a background build finishes, adopt the fresh queue automatically. */
	function watchStatusUntilReady(): void {
		stopWatchingStatus();
		// Svelte fires subscribe synchronously with the current value. We only want
		// to react to FUTURE transitions to 'ready' - a stale 'ready' at subscribe
		// time (e.g. finish()/retryBuild() run while a prior build is still cached)
		// would otherwise adopt the old queue and re-run fetchQueue before the new
		// build starts. init() handles a genuine already-ready status itself.
		let primed = false;
		statusUnsub = discoverQueueStatusStore.subscribe((s) => {
			if (!primed) {
				primed = true;
				return;
			}
			if (phase !== 'building' && phase !== 'finished' && !replacing) return;
			if (s.status === 'ready') {
				void fetchQueue();
			} else if (s.status === 'error') {
				replacing = false;
				phase = queue.length ? 'ready' : 'error';
				errorMessage = s.error ?? 'Queue build failed';
				stopWatchingStatus();
			}
		});
	}

	async function fetchQueue(): Promise<void> {
		stopWatchingStatus();
		const key = requestKey();
		if (!queue.length) phase = 'loading';
		try {
			const data = await api.global.v3.GET(DiscoverV3Api.queue(null), {
				signal: abortController?.signal
			});
			if (key !== requestKey() || abortController?.signal.aborted) return;
			generation++;
			replacing = false;
			queue = dedupeByMbid((data.items ?? []).map(toQueueItem));
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
			discoverQueueStatusStore.markConsumed();
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
			const mbids = queue.map((i) => i.release_group_mbid);
			const data = await api.global.v3.POST(
				DiscoverV3Api.queueValidate(),
				{ release_group_mbids: mbids },
				{ signal: abortController?.signal }
			);
			if (key !== requestKey()) return;
			const inLibrary = new SvelteSet(data.in_library || []);
			if (inLibrary.size > 0) {
				queue = queue.filter((i) => !inLibrary.has(i.release_group_mbid));
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

		const mbid = item.release_group_mbid;
		const key = requestKey();
		const existing = inFlightEnrich.get(mbid);
		if (existing) {
			await existing;
			return;
		}

		const signal = abortController?.signal;
		const promise = (async (): Promise<DiscoverQueueEnrichment | null> => {
			try {
				const data = toQueueEnrichment(
					await api.global.v3.GET(DiscoverV3Api.queueEnrich(mbid), { signal })
				);
				if (key !== requestKey()) return null;
				const idx = queue.findIndex((q) => q.release_group_mbid === mbid);
				if (idx >= 0 && !queue[idx].enrichment) {
					queue[idx] = { ...queue[idx], enrichment: data };
				}
				return data;
			} catch (e) {
				if (isAbortError(e) || key !== requestKey()) return null;
				const idx = queue.findIndex((q) => q.release_group_mbid === mbid);
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
			// clean up any watcher/poll timer left over from a prior init without an
			// intervening destroy (HMR, double-mount) so we don't orphan a poll loop
			stopWatchingStatus();
			discoverQueueStatusStore.reset();
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
			sourceUnsub = subscribeMusicBrainzSourceScope(() => {
				removeQueueCachedData(userId());
				if (isVisible()) void this.init();
				else this.destroy();
			});
			storageUnsub = watchMusicBrainzSourceScope();

			const cached = getQueueCachedData(userId());
			const cachedCurrentMbid = cached
				? cached.data.items[cached.data.currentIndex]?.release_group_mbid
				: undefined;
			const cachedItems = cached ? dedupeByMbid(cached.data.items) : [];
			if (cached && cachedItems.length > 0) {
				queue = cachedItems;
				const resumedIndex = cachedCurrentMbid
					? queue.findIndex((item) => item.release_group_mbid === cachedCurrentMbid)
					: -1;
				currentIndex =
					resumedIndex >= 0
						? resumedIndex
						: Math.max(0, Math.min(cached.data.currentIndex, queue.length - 1));
				queueId = cached.data.queueId;
				phase = 'ready';
				await validateCachedQueue();
				void enrichWindow();
				return;
			}

			phase = 'loading';
			const status = await discoverQueueStatusStore.fetchStatus();
			if (session !== generation) return;
			if (status?.status === 'ready') {
				await fetchQueue();
				return;
			}
			if (status?.status === 'building') {
				phase = 'building';
				watchStatusUntilReady();
				discoverQueueStatusStore.startPolling();
				return;
			}
			if (status?.status === 'idle' && getCacheTTLs().discoverQueueAutoGenerate) {
				phase = 'building';
				watchStatusUntilReady();
				await discoverQueueStatusStore.triggerGenerate(false);
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
			const removedIndex = queue.findIndex((item) => item.release_group_mbid === mbid);
			if (removedIndex < 0) {
				const cached = getQueueCachedData(userId());
				if (!cached) return;
				const items = cached.data.items.filter((item) => item.release_group_mbid !== mbid);
				if (items.length === cached.data.items.length) return;
				if (items.length === 0) {
					removeQueueCachedData(userId());
					return;
				}
				setQueueCachedData(
					{
						...cached.data,
						items,
						currentIndex: Math.min(cached.data.currentIndex, items.length - 1)
					},
					userId()
				);
				return;
			}

			queue = queue.filter((item) => item.release_group_mbid !== mbid);
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
				await api.global.v3.POST(
					DiscoverV3Api.queueIgnore(),
					{
						release_group_mbid: item.release_group_mbid,
						artist_mbid: item.artist_mbid,
						release_name: item.album_name,
						artist_name: item.artist_name
					},
					{ signal: abortController?.signal }
				);
				saved = true;
			} catch {
				// removing it locally is still right even if the ignore write failed
			}
			if (key !== requestKey()) return;
			this.removeByMbid(item.release_group_mbid);
			if (saved) await invalidateDiscoverRecommendations();
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
			removeQueueCachedData(userId());
			phase = 'finished';
			if (getCacheTTLs().discoverQueueAutoGenerate) {
				watchStatusUntilReady();
				void discoverQueueStatusStore.triggerGenerate(false);
			}
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
			watchStatusUntilReady();
			void discoverQueueStatusStore.triggerGenerate(true);
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
			discoverQueueStatusStore.reset();
			inFlightEnrich.clear();
			phase = 'idle';
		}
	};
}

export const discoverQueueDeck = createDiscoverQueueDeck();
