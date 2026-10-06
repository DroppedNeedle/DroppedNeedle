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
import {
	clearPersistedQueryCache,
	createIDBStorage,
	removePersistedQueries,
	type PersistedQueryPredicate
} from './IndexedDbPersister.svelte';
import { createPersistedQueryPersister } from './persistedCache';
import {
	musicBrainzSourceKey,
	subscribeMusicBrainzSourceScope,
	type MusicBrainzSourceKey
} from './musicbrainz/sourceScope.svelte';
import { getDownloadScope, subscribeDownloadRoleChange } from './downloads/downloadScope.svelte';
import { DownloadQueryKeyFactory } from './downloads/DownloadQueryKeyFactory';

const queryPersister = createPersistedQueryPersister(createIDBStorage(isCurrentQueryScope));

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
	if (!isCurrentQueryScope({ queryKey })) return;
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

function providerSourceKey(queryKey: readonly unknown[]): MusicBrainzSourceKey | undefined {
	return queryKey.find(
		(part): part is MusicBrainzSourceKey =>
			typeof part === 'object' &&
			part !== null &&
			'source_mode' in part &&
			typeof part.source_mode === 'string' &&
			'source_id' in part &&
			typeof part.source_id === 'string' &&
			'generation' in part &&
			typeof part.generation === 'number' &&
			'user_id' in part &&
			(typeof part.user_id === 'string' || part.user_id === null)
	);
}

function isCurrentQueryScope(query: { queryKey: readonly unknown[] }): boolean {
	const [root, section, userId, role, generation] = query.queryKey;
	if (root === 'downloads' && section === 'tasks') {
		const current = getDownloadScope();
		return (
			current.userId !== null &&
			userId === current.userId &&
			role === current.role &&
			generation === current.generation
		);
	}
	if (!isMusicBrainzProviderQuery(query)) return true;
	const source = providerSourceKey(query.queryKey);
	const current = musicBrainzSourceKey();
	return (
		source !== undefined &&
		current.source_id !== '' &&
		source.user_id === current.user_id &&
		source.source_mode === current.source_mode &&
		source.source_id === current.source_id &&
		source.generation === current.generation
	);
}

function isMusicBrainzProviderQuery(query: { queryKey: readonly unknown[] }): boolean {
	if (providerSourceKey(query.queryKey)) return true;
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
	const obsolete = {
		predicate: (query: { queryKey: readonly unknown[] }) =>
			isMusicBrainzProviderQuery(query) && !isCurrentQueryScope(query)
	};
	void queryClient.cancelQueries(obsolete);
	queryClient.removeQueries(obsolete);
	void removePersistedQueries(obsolete.predicate).catch(() => undefined);
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

subscribeDownloadRoleChange((userId) => {
	const filters = { queryKey: DownloadQueryKeyFactory.tasksPrefix(userId) };
	// Cancellation fences transport and persister completion before old scope removal.
	void queryClient.cancelQueries(filters);
	queryClient.removeQueries(filters);
	void invalidateQueriesWithPersister(filters, undefined, { removePersisted: true }).catch(
		() => undefined
	);
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
