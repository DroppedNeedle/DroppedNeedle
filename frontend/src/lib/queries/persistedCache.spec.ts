import { describe, expect, it } from 'vitest';
import {
	PERSISTER_KEY_PREFIX,
	type AsyncStorage,
	type PersistedQuery
} from '@tanstack/svelte-query-persist-client';
import { PERSISTED_CACHE_BUSTER, createPersistedQueryPersister } from './persistedCache';

function memoryStorage() {
	const rows = new Map<string, PersistedQuery>();
	const storage: AsyncStorage<PersistedQuery> = {
		getItem: (key) => rows.get(key),
		setItem: (key, value) => {
			rows.set(key, value);
		},
		removeItem: (key) => {
			rows.delete(key);
		},
		entries: () => [...rows.entries()]
	};
	return { storage, rows };
}

function persistedRow(queryKey: readonly unknown[], data: unknown, buster: string): PersistedQuery {
	return {
		buster,
		queryHash: JSON.stringify(queryKey),
		queryKey: [...queryKey],
		state: {
			data,
			dataUpdateCount: 1,
			dataUpdatedAt: Date.now(),
			error: null,
			errorUpdateCount: 0,
			errorUpdatedAt: 0,
			fetchFailureCount: 0,
			fetchFailureReason: null,
			fetchMeta: null,
			isInvalidated: false,
			status: 'success',
			fetchStatus: 'idle'
		}
	};
}

describe('persisted cache version buster', () => {
	it('uses a non-empty buster so v2 rows (buster "") never match', () => {
		expect(PERSISTED_CACHE_BUSTER).toBeTypeOf('string');
		expect(PERSISTED_CACHE_BUSTER.length).toBeGreaterThan(0);
	});

	it('drops a v2 payload instead of hydrating it', async () => {
		const { storage, rows } = memoryStorage();
		const persister = createPersistedQueryPersister(storage);
		const key = ['profile', 'v2-user'] as const;
		const hash = JSON.stringify(key);
		rows.set(`${PERSISTER_KEY_PREFIX}-${hash}`, persistedRow(key, { display_name: 'V2 user' }, ''));

		await expect(persister.retrieveQuery(hash)).resolves.toBeUndefined();
		expect(rows.has(`${PERSISTER_KEY_PREFIX}-${hash}`)).toBe(false);
	});

	it('restores a payload stamped with the current buster', async () => {
		const { storage, rows } = memoryStorage();
		const persister = createPersistedQueryPersister(storage);
		const key = ['profile', 'v3-user'] as const;
		const hash = JSON.stringify(key);
		rows.set(
			`${PERSISTER_KEY_PREFIX}-${hash}`,
			persistedRow(key, { display_name: 'V3 user' }, PERSISTED_CACHE_BUSTER)
		);

		await expect(persister.retrieveQuery(hash)).resolves.toEqual({
			display_name: 'V3 user'
		});
		expect(rows.has(`${PERSISTER_KEY_PREFIX}-${hash}`)).toBe(true);
	});
});
