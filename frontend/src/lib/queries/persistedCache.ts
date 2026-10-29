import {
	type AsyncStorage,
	experimental_createQueryPersister,
	PERSISTER_KEY_PREFIX,
	type PersistedQuery
} from '@tanstack/svelte-query-persist-client';
import {
	notifyManager,
	type Query,
	type QueryFunctionContext,
	type QueryKey
} from '@tanstack/svelte-query';

/** What a query says about its cached answer. */
export interface CacheMeta extends Record<string, unknown> {
	/** Kept in the browser across reloads. Only slow reads opt in. */
	persist?: true;
	/** Built from the MusicBrainz source, so a source switch drops it. */
	provider?: 'musicbrainz';
}

declare module '@tanstack/svelte-query' {
	interface Register {
		queryMeta: CacheMeta;
	}
}

export const PERSIST = { persist: true } as const satisfies CacheMeta;
export const PROVIDER = { provider: 'musicbrainz' } as const satisfies CacheMeta;
export const PERSIST_PROVIDER = {
	persist: true,
	provider: 'musicbrainz'
} as const satisfies CacheMeta;

/** A saved answer. A provider answer names the source it was fetched from. */
export interface StoredQuery extends PersistedQuery {
	provider?: string;
}

/**
 * Cache generation stamp. A row whose buster differs is dropped unread and
 * removed by the clean-up after sign-in. Bump the suffix whenever the key
 * shape changes or a clean cache is needed.
 */
export const PERSISTED_CACHE_BUSTER = 'droppedneedle-v3-5';

/** Maximum age for persisted queries (7 days). */
export const PERSISTED_CACHE_MAX_AGE = 1000 * 60 * 60 * 24 * 7;

const storageKey = (queryHash: string) => `${PERSISTER_KEY_PREFIX}-${queryHash}`;

/**
 * The one persister. A query is saved and restored only when its meta says
 * `persist`; every other query runs as if there were no persister. A provider
 * answer is stamped with the MusicBrainz source it was fetched from and is
 * never restored under another one.
 *
 * TanStack's own restore refreshes a stale answer with a fetch nothing
 * awaits, so a cancel rejects unhandled. The refresh here catches it.
 */
export function createPersistedQueryPersister(
	storage: AsyncStorage<StoredQuery>,
	sourceStamp: () => string
) {
	// Clean-up and prefix removal come from TanStack's persister over the same rows.
	const tanstack = experimental_createQueryPersister<StoredQuery>({
		storage,
		buster: PERSISTED_CACHE_BUSTER,
		maxAge: PERSISTED_CACHE_MAX_AGE,
		serialize: (row) => row,
		deserialize: (row) => row
	});

	const stampFor = (meta: CacheMeta | undefined) =>
		meta?.provider ? `${meta.provider}:${sourceStamp()}` : undefined;

	/** The saved row for a query, or undefined; an expired, old or other-source row is removed. */
	async function read(
		queryHash: string,
		stamp: string | undefined
	): Promise<StoredQuery | undefined> {
		const keyName = storageKey(queryHash);
		try {
			const row = await storage.getItem(keyName);
			if (!row) return undefined;
			const fresh =
				row.buster === PERSISTED_CACHE_BUSTER &&
				row.state.dataUpdatedAt > 0 &&
				Date.now() - row.state.dataUpdatedAt <= PERSISTED_CACHE_MAX_AGE &&
				row.provider === stamp;
			if (fresh) return row;
		} catch {
			// An unreadable row is removed below, as a stale one is.
		}
		await storage.removeItem(keyName);
		return undefined;
	}

	async function save(query: Query, stamp = stampFor(query.meta)): Promise<void> {
		if (query.meta?.persist !== true) return;
		const row: StoredQuery = {
			buster: PERSISTED_CACHE_BUSTER,
			queryHash: query.queryHash,
			queryKey: query.queryKey,
			state: query.state
		};
		if (stamp) row.provider = stamp;
		await storage.setItem(storageKey(query.queryHash), row);
	}

	return {
		read,
		save,
		removeQueries: tanstack.removeQueries,
		persisterGc: tanstack.persisterGc,
		persisterFn: async <T, TQueryKey extends QueryKey>(
			queryFn: (context: QueryFunctionContext<TQueryKey>) => T | Promise<T>,
			context: QueryFunctionContext<TQueryKey>,
			query: Query
		): Promise<T> => {
			const stamp = stampFor(query.meta);
			if (query.meta?.persist !== true) return queryFn(context);
			if (query.state.data === undefined) {
				const row = await read(query.queryHash, stamp);
				if (row) {
					// After the restore lands: keep the saved age, then refresh a stale answer.
					notifyManager.schedule(() => {
						query.setState({
							dataUpdatedAt: row.state.dataUpdatedAt,
							errorUpdatedAt: row.state.errorUpdatedAt
						});
						// eslint-disable-next-line no-restricted-syntax -- TanStack's Query.fetch, not a network call
						if (query.isStale()) void query.fetch().catch(() => undefined);
					});
					return row.state.data as T;
				}
			}
			const data = await queryFn(context);
			// The stamp from before the fetch: an answer that crossed a source switch is never restored.
			notifyManager.schedule(() => void save(query, stamp).catch(() => undefined));
			return data;
		}
	};
}
