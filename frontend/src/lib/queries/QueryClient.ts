import { browser } from '$app/environment';
import {
	type InferDataFromTag,
	type InvalidateOptions,
	type InvalidateQueryFilters,
	MutationCache,
	QueryCache,
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
	musicBrainzSourceStamp,
	subscribeMusicBrainzSourceScope
} from './musicbrainz/sourceScope.svelte';
import { subscribeDownloadRoleChange } from './downloads/downloadScope.svelte';
import { DownloadQueryKeyFactory } from './downloads/DownloadQueryKeyFactory';
import { STALE_DEFAULT } from './staleTime';
import { recordServiceStatus } from '$lib/stores/serviceStatus';

// The MusicBrainz source the cached provider answers were read under, once any was.
let sourceInUse: string | null = null;
const queryPersister = createPersistedQueryPersister(
	createIDBStorage(),
	() => (sourceInUse = musicBrainzSourceStamp())
);

// Both helpers act on the app's client unless given one. A mutation or page
// passes the client it took from context (`useQueryClient()`), so the change
// reaches the cache that rendered it, a spec's own client included.
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
	options?: SetDataOptions,
	client: QueryClient = queryClient
) => {
	await client.setQueryData<TQueryFnData, TTaggedQueryKey, TInferredQueryFnData>(
		queryKey,
		updater,
		options
	);
	const query = client.getQueryCache().find({ queryKey, exact: true });
	if (query) await queryPersister.save(query);
};

export const invalidateQueriesWithPersister = async <TTaggedQueryKey extends QueryKey = QueryKey>(
	filters?: InvalidateQueryFilters<TTaggedQueryKey>,
	options?: InvalidateOptions,
	opts?: {
		removePersisted?: boolean;
		persistedPredicate?: PersistedQueryPredicate;
		client?: QueryClient;
	}
) => {
	// Default keeps IndexedDB rows: queries are marked stale (active ones
	// refetch immediately, inactive ones paint the persisted payload instantly
	// and settle in the background on next mount). Pass `removePersisted: true`
	// only when a stale paint would be actively wrong - it destroys the 7-day
	// persisted-cache benefit for the swept prefix.
	let persistedFailure: unknown;
	let persistedFailed = false;
	if (opts?.removePersisted) {
		// TanStack removes every saved row for a filter without a key, so a
		// filter by predicate names its saved rows itself.
		const persistedRemoval = opts.persistedPredicate
			? removePersistedQueries(opts.persistedPredicate)
			: filters?.queryKey
				? queryPersister.removeQueries(filters)
				: Promise.reject(new Error('Saved rows need a key or a persisted predicate'));
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
		await (opts?.client ?? queryClient).invalidateQueries<TTaggedQueryKey>(filters, options);
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

const isProviderQuery = (query: { meta?: { provider?: string } }) =>
	query.meta?.provider === 'musicbrainz';

/**
 * Refreshes every answer built from the MusicBrainz source and removes their
 * saved copies. Other data stays.
 */
export const invalidateMusicBrainzProviderQueries = async (): Promise<void> => {
	await invalidateQueriesWithPersister({ predicate: isProviderQuery }, undefined, {
		removePersisted: true,
		persistedPredicate: (row) => row.provider !== undefined
	});
};

// A source switch resets the provider answers in memory, so the screens ask
// again, and removes saved answers of any other source. A saved answer names
// its source, so one from before the switch is never restored after it.
subscribeMusicBrainzSourceScope((next) => {
	const stamp = musicBrainzSourceStamp();
	if (next.userId === null || sourceInUse === null || stamp === sourceInUse) return;
	sourceInUse = stamp;
	void queryClient.resetQueries({ predicate: isProviderQuery }).catch(() => undefined);
	void removePersistedQueries(
		(row) => row.provider !== undefined && row.provider !== `musicbrainz:${stamp}`
	).catch(() => undefined);
});

export const queryClient = new QueryClient({
	// A degraded answer names its sources in `service_status`; this is the one place that reads it.
	queryCache: new QueryCache({ onSuccess: recordServiceStatus }),
	mutationCache: new MutationCache({ onSuccess: recordServiceStatus }),
	defaultOptions: {
		queries: {
			enabled: browser,
			retry: false,
			refetchOnWindowFocus: true,
			staleTime: STALE_DEFAULT,
			gcTime: 1000 * 60 * 5, // 5 min: keep results in memory so back-nav is instant (30s evicted before staleTime, forcing a skeleton + IDB rehydrate each return)
			persister: queryPersister.persisterFn
		}
	}
});

subscribeDownloadRoleChange((userId) => {
	const filters = { queryKey: DownloadQueryKeyFactory.tasksPrefix(userId) };
	// Cancelling first keeps a late answer for the old role out of the cache.
	void queryClient.cancelQueries(filters);
	queryClient.removeQueries(filters);
});

/**
 * Drop ALL cached query data on login / logout / user-switch. Keys are scoped
 * by user, so this is the backstop, not the only guard.
 */
export const resetQueryCacheForUserSwitch = async (): Promise<void> => {
	queryClient.clear();
	await clearPersistedQueryCache();
};

// Long enough for the first page to paint from the saved answers first.
const GC_DELAY_MS = 5_000;
let collectedFor: string | null = null;

/**
 * Removes saved answers past their age or from an older cache version, once
 * per signed-in user (at start, and after each sign-in).
 */
export function collectPersistedGarbage(userId: string): void {
	if (collectedFor === userId) return;
	collectedFor = userId;
	setTimeout(() => void queryPersister.persisterGc().catch(() => undefined), GC_DELAY_MS);
}
