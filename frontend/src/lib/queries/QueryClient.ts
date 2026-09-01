import { browser } from '$app/environment';
import {
	type InferDataFromTag,
	type InvalidateOptions,
	type InvalidateQueryFilters,
	QueryClient,
	type QueryKey,
	type SetDataOptions,
	type Updater
} from '@tanstack/svelte-query';
import { experimental_createQueryPersister } from '@tanstack/svelte-query-persist-client';
import {
	clearPersistedQueryCache,
	createIDBStorage,
	removePersistedQueries,
	type PersistedQueryPredicate
} from './IndexedDbPersister.svelte';
import { subscribeMusicBrainzSourceScope } from './musicbrainz/sourceScope.svelte';

/**
 * Maximum age for queries to be persisted.
 * @see https://tanstack.com/query/latest/docs/framework/react/plugins/persistQueryClient#how-it-works
 */
const QUERY_MAX_AGE = 1000 * 60 * 60 * 24 * 7; // 7 days

const queryPersister = experimental_createQueryPersister({
	storage: createIDBStorage(),
	maxAge: QUERY_MAX_AGE,
	// No need to serialize/deserialize since we're using IndexedDB which can store complex objects.
	serialize: (persistedQuery) => persistedQuery,
	deserialize: (cached) => cached
});

export const setQueryDataWithPersister = async <
	TQueryFnData = unknown,
	TTaggedQueryKey extends QueryKey = QueryKey,
	TInferredQueryFnData = InferDataFromTag<TQueryFnData, TTaggedQueryKey>
>(
	queryKey: TTaggedQueryKey,
	updater: Updater<
		NoInfer<TInferredQueryFnData> | undefined,
		NoInfer<TInferredQueryFnData> | undefined
	>,
	options?: SetDataOptions
) => {
	// eslint-disable-next-line no-restricted-syntax
	await queryClient.setQueryData<TQueryFnData, TTaggedQueryKey, TInferredQueryFnData>(
		queryKey,
		updater,
		options
	);
	await queryPersister.persistQueryByKey(queryKey, queryClient);
};

export const invalidateQueriesWithPersister = async <TTaggedQueryKey extends QueryKey = QueryKey>(
	filters?: InvalidateQueryFilters<TTaggedQueryKey>,
	options?: InvalidateOptions,
	opts?: { removePersisted?: boolean; persistedPredicate?: PersistedQueryPredicate }
) => {
	// Default keeps IndexedDB rows: queries are marked stale (active ones
	// refetch immediately, inactive ones paint the persisted payload instantly
	// and settle in the background on next mount). Pass `removePersisted: true`
	// only when a stale paint would be actively wrong - it destroys the 7-day
	// persisted-cache benefit for the swept prefix.
	let persistedFailure: unknown;
	let persistedFailed = false;
	if (opts?.removePersisted) {
		const persistedRemoval = opts.persistedPredicate
			? removePersistedQueries(opts.persistedPredicate)
			: queryPersister.removeQueries(filters);
		try {
			await persistedRemoval;
		} catch (error) {
			persistedFailure = error;
			persistedFailed = true;
		}
	}

	let activeFailure: unknown;
	let activeFailed = false;
	try {
		// eslint-disable-next-line no-restricted-syntax
		await queryClient.invalidateQueries<TTaggedQueryKey>(filters, options);
	} catch (error) {
		activeFailure = error;
		activeFailed = true;
	}

	if (persistedFailed && activeFailed) {
		throw new AggregateError([persistedFailure, activeFailure], 'Query cache invalidation failed');
	}
	if (persistedFailed) throw persistedFailure;
	if (activeFailed) throw activeFailure;
};

const MUSICBRAINZ_ARTIST_QUERY_SEGMENTS: Record<string, true> = {
	extended: true,
	releases: true
};
const MUSICBRAINZ_SEARCH_QUERY_SEGMENTS: Record<string, true> = {
	artists: true,
	albums: true,
	suggestions: true
};
const MUSICBRAINZ_DISCOVER_QUERY_SEGMENTS: Record<string, true> = {
	radio: true,
	'playlist-suggestions': true
};

function isMusicBrainzProviderQuery(query: { queryKey: readonly unknown[] }): boolean {
	const [root, second, third, fourth] = query.queryKey;
	if (root === 'artist') {
		if (query.queryKey.length === 2) return true;
		if (typeof second === 'object' && second !== null) {
			if (query.queryKey.length === 3) return true;
			return typeof fourth === 'string' && MUSICBRAINZ_ARTIST_QUERY_SEGMENTS[fourth] === true;
		}
		return typeof third === 'string' && MUSICBRAINZ_ARTIST_QUERY_SEGMENTS[third] === true;
	}
	if (root === 'albums') return second === 'editions';
	if (root === 'search') {
		const segment = typeof third === 'string' ? third : fourth;
		return typeof segment === 'string' && MUSICBRAINZ_SEARCH_QUERY_SEGMENTS[segment] === true;
	}
	if (root === 'discover') {
		const segment = typeof third === 'string' ? third : fourth;
		// The home response intentionally mixes library/user sections with provider-derived
		// recommendations, so its whole user-keyed payload is a correctness boundary.
		if (typeof segment !== 'string') {
			return query.queryKey.length === 2 || (typeof third === 'object' && third !== null);
		}
		return MUSICBRAINZ_DISCOVER_QUERY_SEGMENTS[segment] === true;
	}
	if (root === 'home') {
		// Home's source-scoped payload mixes provider recommendations with local sections.
		return typeof third === 'object' && third !== null;
	}
	return false;
}

/**
 * A MusicBrainz source switch invalidates provider-bearing artist/album/search/discovery
 * queries. The discover home response is deliberately swept whole because it mixes
 * provider recommendations with user/library sections; local search, discovery batches,
 * integrations, and other user data stay outside this boundary. Persisted provider rows
 * are removed because their source provenance is no longer valid.
 */
export const invalidateMusicBrainzProviderQueries = async (): Promise<void> => {
	await invalidateQueriesWithPersister({ predicate: isMusicBrainzProviderQuery }, undefined, {
		removePersisted: true,
		persistedPredicate: isMusicBrainzProviderQuery
	});
};
subscribeMusicBrainzSourceScope((next, previous) => {
	if (
		next.userId === null ||
		next.userId !== previous.userId ||
		(next.sourceMode === previous.sourceMode &&
			next.sourceId === previous.sourceId &&
			next.generation === previous.generation)
	) {
		return;
	}
	void invalidateMusicBrainzProviderQueries().catch(() => undefined);
});

export const queryClient = new QueryClient({
	defaultOptions: {
		queries: {
			enabled: browser,
			retry: false,
			refetchOnWindowFocus: true,
			staleTime: 1000 * 60 * 1, // 1 minute
			gcTime: 1000 * 60 * 5, // 5 min: keep results in memory so back-nav is instant (30s evicted before staleTime, forcing a skeleton + IDB rehydrate each return)
			persister: queryPersister.persisterFn
		}
	}
});

/**
 * Drop ALL cached query data on login / logout / user-switch (AMU-5): QueryClient +
 * IndexedDB persister form one browser-wide cache with no user dimension, so
 * personalized data would otherwise leak across users sharing a browser.
 */
export const resetQueryCacheForUserSwitch = async (): Promise<void> => {
	queryClient.clear();
	await clearPersistedQueryCache();
};
