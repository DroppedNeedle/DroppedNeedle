import {
	type AsyncStorage,
	PERSISTER_KEY_PREFIX,
	type PersistedQuery
} from '@tanstack/svelte-query-persist-client';
import { clear, del, entries, get, set } from 'idb-keyval';
/**
 * Wipe every persisted query from IndexedDB on a user switch (AMU-5): the
 * per-query entries written by {@link createIDBStorage}. idb-keyval's default
 * store is used only by this persister (verified), so a blanket `clear()` is safe
 * and cannot drop unrelated app data.
 */
export async function clearPersistedQueryCache(): Promise<void> {
	await clear();
}

export type PersistedQueryPredicate = (query: Pick<PersistedQuery, 'queryKey'>) => boolean;

function asPersistedQuery(value: unknown): PersistedQuery | null {
	if (typeof value !== 'object' || value === null) return null;
	const candidate = value as Partial<PersistedQuery>;
	if (
		typeof candidate.buster !== 'string' ||
		typeof candidate.queryHash !== 'string' ||
		!Array.isArray(candidate.queryKey) ||
		typeof candidate.state !== 'object' ||
		candidate.state === null
	) {
		return null;
	}
	return candidate as PersistedQuery;
}

/**
 * Remove only persisted query rows whose decoded query key matches `predicate`.
 * Rows outside the persister key namespace and malformed rows are retained.
 */
export async function removePersistedQueries(predicate: PersistedQueryPredicate): Promise<void> {
	const storageKeyPrefix = `${PERSISTER_KEY_PREFIX}-`;
	const storedEntries = await entries<string, unknown>();
	let firstFailure: unknown;
	let removalFailed = false;

	for (const [key, value] of storedEntries) {
		if (typeof key !== 'string' || !key.startsWith(storageKeyPrefix)) continue;
		const persistedQuery = asPersistedQuery(value);
		if (!persistedQuery) continue;

		let matches = false;
		try {
			matches = predicate(persistedQuery);
		} catch {
			continue;
		}
		if (!matches) continue;

		try {
			await del(key);
		} catch (error) {
			if (!removalFailed) firstFailure = error;
			removalFailed = true;
		}
	}

	if (removalFailed) throw firstFailure;
}

export function createIDBStorage(): AsyncStorage<PersistedQuery> {
	return {
		getItem: async (key: string) => {
			const val = await get<PersistedQuery>(key);
			return val;
		},
		setItem: async (key: string, value: PersistedQuery) => {
			// In some cases, a svelte state proxy value appears in the query state, which cannot be stored in IndexedDB.
			// To work around this, we can snapshot the value before storing it.
			try {
				await set(key, $state.snapshot(value));
			} catch (e) {
				console.error('Failed to set item in IndexedDB', key, value, e);
				throw e;
			}
		},
		removeItem: async (key: string) => {
			await del(key);
		},
		entries: async () => {
			return await entries();
		}
	};
}
