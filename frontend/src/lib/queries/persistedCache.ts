import {
	type AsyncStorage,
	experimental_createQueryPersister,
	type PersistedQuery
} from '@tanstack/svelte-query-persist-client';

/**
 * Cache generation stamp. TanStack drops any persisted row whose buster
 * differs, so this single string invalidates every v2 payload on first v3
 * boot (v2 persisted with the default empty buster, and v3 reassigns user
 * IDs, so stale rows would corrupt rather than help). Bump the suffix to
 * force a clean cache in a future release.
 */
export const PERSISTED_CACHE_BUSTER = 'droppedneedle-v3-1';

/** Maximum age for persisted queries (7 days). */
export const PERSISTED_CACHE_MAX_AGE = 1000 * 60 * 60 * 24 * 7;

/**
 * The one persister factory: IndexedDB storage keeps complex objects as-is,
 * so serialization is the identity. Both the app query client and the
 * cache-buster brief build through here, which keeps the brief honest.
 */
export function createPersistedQueryPersister(storage: AsyncStorage<PersistedQuery>) {
	return experimental_createQueryPersister({
		storage,
		buster: PERSISTED_CACHE_BUSTER,
		maxAge: PERSISTED_CACHE_MAX_AGE,
		serialize: (persistedQuery) => persistedQuery,
		deserialize: (cached) => cached
	});
}
