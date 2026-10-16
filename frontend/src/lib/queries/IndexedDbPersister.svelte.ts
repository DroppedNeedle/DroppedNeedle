import {
	type AsyncStorage,
	PERSISTER_KEY_PREFIX,
	type PersistedQuery
} from '@tanstack/svelte-query-persist-client';
import { clear, createStore, del, entries, get } from 'idb-keyval';
/**
 * Wipe every persisted query from IndexedDB on a user switch: the
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

const persistedStore = createStore('keyval-store', 'keyval');
const REMOVAL_BATCH_SIZE = 128;

/**
 * Remove only persisted query rows whose decoded query key matches `predicate`.
 * Rows outside the persister key namespace and malformed rows are retained.
 */
export async function removePersistedQueries(predicate: PersistedQueryPredicate): Promise<void> {
	const storageKeyPrefix = `${PERSISTER_KEY_PREFIX}-`;
	let firstFailure: unknown;
	let removalFailed = false;
	const recordFailure = (error: unknown) => {
		if (!removalFailed) firstFailure = error;
		removalFailed = true;
	};

	const sweep = async (
		after: IDBValidKey | undefined,
		until: IDBValidKey | undefined,
		batchSize: number
	): Promise<{ last: IDBValidKey | undefined; exhausted: boolean; aborted: boolean }> =>
		persistedStore('readwrite', (store) => {
			const { promise, resolve } = Promise.withResolvers<{
				last: IDBValidKey | undefined;
				exhausted: boolean;
				aborted: boolean;
			}>();
			let last = after;
			let visited = 0;
			let exhausted = false;
			const range =
				after === undefined
					? until === undefined
						? undefined
						: IDBKeyRange.upperBound(until)
					: until === undefined
						? IDBKeyRange.lowerBound(after, true)
						: IDBKeyRange.bound(after, until, true);
			const request = store.openCursor(range);
			request.onsuccess = () => {
				const cursor = request.result;
				if (!cursor) {
					exhausted = true;
					return;
				}
				last = cursor.key;
				visited++;
				if (typeof cursor.key === 'string' && cursor.key.startsWith(storageKeyPrefix)) {
					const query = asPersistedQuery(cursor.value);
					let matches = false;
					try {
						matches = query !== null && predicate(query);
					} catch {
						// A malformed predicate result must not remove the row.
					}
					if (matches) {
						try {
							const deletion = store.delete(cursor.key);
							deletion.onerror = (event) => {
								recordFailure(deletion.error);
								event.preventDefault();
								event.stopPropagation();
							};
						} catch (error) {
							recordFailure(error);
						}
					}
				}
				if (visited < batchSize) {
					try {
						cursor.continue();
					} catch (error) {
						recordFailure(error);
					}
				}
			};
			store.transaction.oncomplete = () => resolve({ last, exhausted, aborted: false });
			store.transaction.onabort = () => {
				recordFailure(store.transaction.error);
				resolve({ last, exhausted, aborted: true });
			};
			return promise;
		});

	let after: IDBValidKey | undefined;
	while (true) {
		const batch = await sweep(after, undefined, REMOVAL_BATCH_SIZE);
		if (batch.aborted && batch.last !== undefined) {
			// An abort rolls back successful requests too; retry that range row by row.
			let retryAfter = after;
			while (retryAfter === undefined || indexedDB.cmp(retryAfter, batch.last) < 0) {
				const retried = await sweep(retryAfter, batch.last, 1);
				if (retried.last === retryAfter) break;
				retryAfter = retried.last;
				if (retried.exhausted) break;
			}
		}
		if (batch.exhausted || batch.last === after) break;
		after = batch.last;
	}
	if (removalFailed) throw firstFailure;
}

export function createIDBStorage(
	canPersist: PersistedQueryPredicate
): AsyncStorage<PersistedQuery> {
	return {
		getItem: async (key: string) => {
			const val = await get<PersistedQuery>(key);
			return val && canPersist(val) ? val : undefined;
		},
		setItem: async (key: string, value: PersistedQuery) => {
			await persistedStore('readwrite', (store) => {
				// Check at transaction admission, not before awaiting the database open.
				if (!canPersist(value)) return Promise.resolve();
				return new Promise<void>((resolve, reject) => {
					store.transaction.oncomplete = () => resolve();
					store.transaction.onabort = () => reject(store.transaction.error);
					store.put($state.snapshot(value), key);
				});
			});
		},
		removeItem: async (key: string) => {
			await del(key);
		},
		entries: async () => {
			return await entries();
		}
	};
}
