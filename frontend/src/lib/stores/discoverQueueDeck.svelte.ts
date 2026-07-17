/**
 * State machine for the always-visible Discover Queue deck.
 *
 * Load order favours instant paint: localStorage queue (resume) -> background-built
 * queue via GET /queue (already enriched + cover-prewarmed) -> inline build fallback.
 * The queue is consumed-once and mutated locally (advance/ignore/jump), so it lives
 * here rather than in TanStack Query; every mutation persists to localStorage.
 */
import { API } from '$lib/constants';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { discoverQueueStatusStore } from '$lib/stores/discoverQueueStatus';
import { getCacheTTLs } from '$lib/stores/cacheTtl';
import {
	getQueueCachedData,
	removeQueueCachedData,
	setQueueCachedData
} from '$lib/utils/discoverQueueCache';
import { isAbortError } from '$lib/utils/errorHandling';
import { invalidateDiscoverRecommendations } from '$lib/queries/discover/DiscoverInvalidation';
import { SvelteMap } from 'svelte/reactivity';
import type {
	DiscoverQueueEnrichment,
	DiscoverQueueItemFull,
	DiscoverQueueResponse
} from '$lib/types';

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

function createDiscoverQueueDeck() {
	let phase = $state<DeckPhase>('idle');
	let queue = $state<DiscoverQueueItemFull[]>([]);
	let currentIndex = $state(0);
	let queueId = $state('');
	let errorMessage = $state('');

	let abortController: AbortController | null = null;
	let statusUnsub: (() => void) | null = null;
	const inFlightEnrich = new SvelteMap<string, Promise<DiscoverQueueEnrichment | null>>();

	const current = $derived(queue[currentIndex]);
	const isLast = $derived(currentIndex >= queue.length - 1);

	function userId(): string {
		return authStore.user?.id ?? 'anon';
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
			if (phase !== 'building' && phase !== 'finished') return;
			if (s.status === 'ready') {
				void fetchQueue();
			} else if (s.status === 'error' && phase === 'building') {
				phase = 'error';
				errorMessage = s.error ?? 'Queue build failed';
				stopWatchingStatus();
			}
		});
	}

	async function fetchQueue(): Promise<void> {
		stopWatchingStatus();
		phase = 'loading';
		try {
			const data = await api.global.get<DiscoverQueueResponse>(API.discoverQueue(), {
				signal: abortController?.signal
			});
			queue = data.items.map((item) => ({ ...item }));
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
			if (getCacheTTLs().discoverQueueAutoGenerate) {
				void discoverQueueStatusStore.triggerGenerate(false);
			}
		} catch (e) {
			if (isAbortError(e)) return;
			phase = 'error';
			errorMessage = 'Could not load your discovery queue';
		}
	}

	async function validateCachedQueue(): Promise<void> {
		if (queue.length === 0) return;
		try {
			const mbids = queue.map((i) => i.release_group_mbid);
			const data = await api.global.post<{ in_library?: string[] }>(
				API.discoverQueueValidate(),
				{ release_group_mbids: mbids },
				{ signal: abortController?.signal }
			);
			const inLibrary = new Set(data.in_library || []);
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
		const existing = inFlightEnrich.get(mbid);
		if (existing) {
			await existing;
			return;
		}

		const signal = abortController?.signal;
		const promise = (async (): Promise<DiscoverQueueEnrichment | null> => {
			try {
				const data = await api.global.get<DiscoverQueueEnrichment>(API.discoverQueueEnrich(mbid), {
					signal
				});
				const idx = queue.findIndex((q) => q.release_group_mbid === mbid);
				if (idx >= 0 && !queue[idx].enrichment) {
					queue[idx] = { ...queue[idx], enrichment: data };
				}
				return data;
			} catch (e) {
				if (isAbortError(e)) return null;
				const idx = queue.findIndex((q) => q.release_group_mbid === mbid);
				if (idx >= 0 && !queue[idx].enrichment) {
					queue[idx] = { ...queue[idx], enrichment: emptyEnrichment() };
				}
				return null;
			} finally {
				inFlightEnrich.delete(mbid);
			}
		})();
		inFlightEnrich.set(mbid, promise);
		await promise;
	}

	async function enrichWindow(): Promise<void> {
		if (queue.length === 0) return;
		await enrichItem(currentIndex);
		for (let i = 1; i <= 2; i++) {
			if (currentIndex + i < queue.length) {
				void enrichItem(currentIndex + i);
			}
		}
	}

	return {
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

		async init(): Promise<void> {
			abortController?.abort();
			abortController = new AbortController();
			// clean up any watcher/poll timer left over from a prior init without an
			// intervening destroy (HMR, double-mount) so we don't orphan a poll loop
			stopWatchingStatus();
			discoverQueueStatusStore.stopPolling();

			const cached = getQueueCachedData(userId());
			if (cached && cached.data.items.length > 0) {
				queue = cached.data.items;
				currentIndex = Math.min(cached.data.currentIndex, cached.data.items.length - 1);
				queueId = cached.data.queueId;
				phase = 'ready';
				await validateCachedQueue();
				void enrichWindow();
				return;
			}

			phase = 'loading';
			const status = await discoverQueueStatusStore.fetchStatus();
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
			let saved = false;
			try {
				await api.global.post(
					API.discoverQueueIgnore(),
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
			phase = 'building';
			watchStatusUntilReady();
			void discoverQueueStatusStore.triggerGenerate(true);
		},

		/** Slow path: build inline right now instead of waiting on the background task. */
		buildNow(): void {
			void fetchQueue();
		},

		destroy(): void {
			abortController?.abort();
			abortController = null;
			stopWatchingStatus();
			discoverQueueStatusStore.stopPolling();
			inFlightEnrich.clear();
			phase = 'idle';
		}
	};
}

export const discoverQueueDeck = createDiscoverQueueDeck();
