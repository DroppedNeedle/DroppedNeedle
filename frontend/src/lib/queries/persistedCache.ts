import {
	type AsyncStorage,
	experimental_createQueryPersister,
	type PersistedQuery
} from '@tanstack/svelte-query-persist-client';
import {
	notifyManager,
	type Query,
	type QueryFunctionContext,
	type QueryKey
} from '@tanstack/svelte-query';

/**
 * Cache generation stamp. TanStack drops any persisted row whose buster
 * differs, so this single string invalidates every v2 payload on first v3
 * boot (v2 persisted with the default empty buster, and v3 reassigns user
 * IDs, so stale rows would corrupt rather than help). Bump the suffix to
 * force a clean cache in a future release.
 */
export const PERSISTED_CACHE_BUSTER = 'droppedneedle-v3-4';

/** Maximum age for persisted queries (7 days). */
export const PERSISTED_CACHE_MAX_AGE = 1000 * 60 * 60 * 24 * 7;

/**
 * The one persister factory: IndexedDB storage keeps complex objects as-is,
 * so serialization is the identity. Both the app query client and the
 * cache-buster brief build through here, which keeps the brief honest.
 *
 * TanStack's persister refreshes a stale restored query with a fetch that
 * nothing awaits, so an invalidation or unmount that cancels it rejects
 * unhandled. This persister turns that refresh off and starts the same refresh
 * below with its rejection caught.
 */
export function createPersistedQueryPersister(storage: AsyncStorage<PersistedQuery>) {
	const persister = experimental_createQueryPersister({
		storage,
		buster: PERSISTED_CACHE_BUSTER,
		maxAge: PERSISTED_CACHE_MAX_AGE,
		serialize: (persistedQuery) => persistedQuery,
		deserialize: (cached) => cached,
		refetchOnRestore: false
	});
	return {
		...persister,
		persisterFn: async <T, TQueryKey extends QueryKey>(
			queryFn: (context: QueryFunctionContext<TQueryKey>) => T | Promise<T>,
			context: QueryFunctionContext<TQueryKey>,
			query: Query
		): Promise<T> => {
			let fetched = false;
			const data = await persister.persisterFn(
				(ctx) => {
					fetched = true;
					return queryFn(ctx);
				},
				context,
				query
			);
			if (!fetched) {
				// Scheduled after TanStack's restore step, which resets the restored timestamp.
				notifyManager.schedule(() => {
					// eslint-disable-next-line no-restricted-syntax -- TanStack's Query.fetch, not a network call
					if (query.isStale()) void query.fetch().catch(() => undefined);
				});
			}
			return data;
		}
	};
}
