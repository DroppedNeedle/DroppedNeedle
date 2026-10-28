import { writable, get } from 'svelte/store';
import type { Artist, Album } from '$lib/types';
import type { EnrichmentSource } from '$lib/types';
import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
import { clearLocalStorageNamespace, createLocalStorageCache } from '$lib/utils/localStorageCache';

interface SearchCache {
	query: string;
	artists: Artist[];
	albums: Album[];
	topArtist: Artist | null;
	topAlbum: Album | null;
	timestamp: number;
	enrichmentSource: EnrichmentSource;
}

const searchCacheTTL = CACHE_TTL.SEARCH;

const persistentSearchCache = createLocalStorageCache<Omit<SearchCache, 'timestamp'>>(
	CACHE_KEYS.SEARCH,
	CACHE_TTL.SEARCH,
	{ maxEntries: 60 }
);

function normalizeQuery(query: string): string {
	return query.trim().toLowerCase();
}

function getCacheSuffix(query: string): string {
	return encodeURIComponent(normalizeQuery(query));
}

function isCacheStale(timestamp: number): boolean {
	return Date.now() - timestamp > searchCacheTTL;
}

function hydratePersistentCache(query: string): SearchCache | null {
	const normalizedQuery = normalizeQuery(query);
	if (!normalizedQuery) return null;

	const stored = persistentSearchCache.get(getCacheSuffix(normalizedQuery));
	if (!stored) return null;

	return {
		query: stored.data.query,
		artists: stored.data.artists,
		albums: stored.data.albums,
		topArtist: stored.data.topArtist ?? null,
		topAlbum: stored.data.topAlbum ?? null,
		enrichmentSource: stored.data.enrichmentSource,
		timestamp: stored.timestamp
	};
}

function persistCache(cache: SearchCache): void {
	persistentSearchCache.set(
		{
			query: cache.query,
			artists: cache.artists,
			albums: cache.albums,
			topArtist: cache.topArtist,
			topAlbum: cache.topAlbum,
			enrichmentSource: cache.enrichmentSource
		},
		getCacheSuffix(cache.query)
	);
}

function createSearchStore() {
	const { subscribe, set, update } = writable<SearchCache | null>(null);

	return {
		subscribe,
		setResults(
			query: string,
			artists: Artist[],
			albums: Album[],
			enrichmentSource: EnrichmentSource = null,
			topArtist: Artist | null = null,
			topAlbum: Album | null = null
		) {
			const normalizedQuery = normalizeQuery(query);
			const cache: SearchCache = {
				query: normalizedQuery,
				artists,
				albums,
				topArtist,
				topAlbum,
				timestamp: Date.now(),
				enrichmentSource
			};
			set(cache);
			persistCache(cache);
		},
		updateArtists(artists: Artist[]) {
			update((cache) => {
				if (cache) {
					const updatedCache: SearchCache = {
						...cache,
						artists,
						timestamp: Date.now()
					};
					persistCache(updatedCache);
					return updatedCache;
				}
				return cache;
			});
		},
		updateAlbums(albums: Album[]) {
			update((cache) => {
				if (cache) {
					const updatedCache: SearchCache = {
						...cache,
						albums,
						timestamp: Date.now()
					};
					persistCache(updatedCache);
					return updatedCache;
				}
				return cache;
			});
		},
		setEnrichmentSource(enrichmentSource: EnrichmentSource) {
			update((cache) => {
				if (cache) {
					const updatedCache: SearchCache = {
						...cache,
						enrichmentSource,
						timestamp: Date.now()
					};
					persistCache(updatedCache);
					return updatedCache;
				}
				return cache;
			});
		},
		getCache(query: string, options: { allowStale?: boolean } = {}): SearchCache | null {
			const normalizedQuery = normalizeQuery(query);
			const allowStale = options.allowStale ?? false;
			const cache = get({ subscribe });
			if (cache && cache.query === normalizedQuery) {
				if (!allowStale && isCacheStale(cache.timestamp)) {
					return null;
				}
				return cache;
			}

			const persistentCache = hydratePersistentCache(normalizedQuery);
			if (!persistentCache) {
				return null;
			}

			if (!allowStale && isCacheStale(persistentCache.timestamp)) {
				return null;
			}

			set(persistentCache);
			return persistentCache;
		},
		isStale(timestamp: number): boolean {
			return isCacheStale(timestamp);
		},
		clear() {
			set(null);
			clearLocalStorageNamespace(CACHE_KEYS.SEARCH);
		}
	};
}

export const searchStore = createSearchStore();
