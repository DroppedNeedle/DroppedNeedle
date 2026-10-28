import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
import { clearLocalStorageNamespace, createLocalStorageCache } from '$lib/utils/localStorageCache';
import type { DeckCard } from '$lib/queries/discover/types';
import { musicBrainzSourceKey } from '$lib/queries/musicbrainz/sourceScope.svelte';
import type { MusicBrainzSourceIdentity } from '$lib/stores/authStore.svelte';

export interface QueueCacheData {
	items: DeckCard[];
	currentIndex: number;
	queueId: string;
}

type QueueCacheEnvelope = QueueCacheData & {
	version: 2;
	scope: MusicBrainzSourceIdentity & { user_id: string | null };
};

const queueCache = createLocalStorageCache<QueueCacheEnvelope>(
	CACHE_KEYS.DISCOVER_QUEUE,
	CACHE_TTL.DISCOVER_QUEUE
);

const QUEUE_CACHE_EVENT = 'discover-queue-cache-changed';

function notifyQueueCacheChanged(): void {
	if (typeof window === 'undefined') return;
	window.dispatchEvent(new CustomEvent(QUEUE_CACHE_EVENT));
}

export function subscribeQueueCacheChanges(listener: () => void): () => void {
	if (typeof window === 'undefined') return () => {};

	const handler = () => listener();
	window.addEventListener(QUEUE_CACHE_EVENT, handler);
	return () => {
		window.removeEventListener(QUEUE_CACHE_EVENT, handler);
	};
}

// Legacy and other source generations must never resume a consumed deck.
export const getQueueCachedData = (userId: string) => {
	const cached = queueCache.get(userId);
	if (!cached) return null;

	if (
		cached.data.version !== 2 ||
		!cached.data.scope?.source_id ||
		JSON.stringify(cached.data.scope) !== JSON.stringify(musicBrainzSourceKey(userId)) ||
		queueCache.isStale(cached.timestamp)
	) {
		queueCache.remove(userId);
		notifyQueueCacheChanged();
		return null;
	}

	return cached;
};

export const setQueueCachedData = (data: QueueCacheData, userId: string) => {
	const scope = musicBrainzSourceKey(userId);
	if (!scope.source_id) return;
	queueCache.set({ ...data, version: 2, scope }, userId);
	notifyQueueCacheChanged();
};

export const removeQueueCachedData = (userId: string) => {
	queueCache.remove(userId);
	notifyQueueCacheChanged();
};
export const updateDiscoverQueueCacheTTL = queueCache.updateTTL;

export function removeAllQueueCachedData(): void {
	clearLocalStorageNamespace(CACHE_KEYS.DISCOVER_QUEUE);
	notifyQueueCacheChanged();
}
