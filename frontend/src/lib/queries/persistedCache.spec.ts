import { describe, expect, it } from 'vitest';
import { QueryClient } from '@tanstack/svelte-query';
import { PERSISTER_KEY_PREFIX, type AsyncStorage } from '@tanstack/svelte-query-persist-client';
import {
	PERSIST,
	PERSIST_PROVIDER,
	PERSISTED_CACHE_BUSTER,
	createPersistedQueryPersister,
	type StoredQuery
} from './persistedCache';
import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';
import { LibraryQueryKeyFactory } from './library/LibraryQueryKeyFactory';
import { RequestQueryKeyFactory } from './requests/RequestQueryKeyFactory';

function memoryStorage() {
	const rows = new Map<string, StoredQuery>();
	const storage: AsyncStorage<StoredQuery> = {
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

function storedRow(queryKey: readonly unknown[], data: unknown, buster: string): StoredQuery {
	return {
		buster,
		queryHash: JSON.stringify(queryKey),
		queryKey,
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

function setup(stamp = () => 'source-a') {
	const { storage, rows } = memoryStorage();
	const persister = createPersistedQueryPersister(storage, stamp);
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false, persister: persister.persisterFn } }
	});
	return { rows, persister, client };
}

const settled = () => new Promise((resolve) => setTimeout(resolve, 10));

describe('the browser copy of query answers', () => {
	it('keeps nothing of a query that does not ask to persist', async () => {
		const { rows, client } = setup();
		await client.fetchQuery({
			queryKey: RequestQueryKeyFactory.active('a'),
			queryFn: () => ({ items: [1] })
		});
		await settled();
		expect(rows.size).toBe(0);
	});

	it('keeps a query that asks to persist', async () => {
		const { rows, client } = setup();
		await client.fetchQuery({
			queryKey: LibraryQueryKeyFactory.catalog.stats('a'),
			meta: PERSIST,
			queryFn: () => ({ albums: [1] })
		});
		await settled();
		expect([...rows.values()].map((row) => row.state.data)).toEqual([{ albums: [1] }]);
	});

	it('never restores a provider answer under another MusicBrainz source', async () => {
		let source = 'source-a';
		const { rows, persister, client } = setup(() => source);
		const queryKey = HomeQueryKeyFactory.home('a');
		await client.fetchQuery({ queryKey, meta: PERSIST_PROVIDER, queryFn: () => 'from a' });
		await settled();
		const [row] = [...rows.values()];
		expect(row.provider).toBe('musicbrainz:source-a');

		source = 'source-b';
		await expect(persister.read(row.queryHash, 'musicbrainz:source-b')).resolves.toBeUndefined();
		expect(rows.size).toBe(0);
	});

	it('drops a row from an older cache version instead of restoring it', async () => {
		const { rows, persister } = setup();
		const row = storedRow(HomeQueryKeyFactory.home('v2-user'), { display_name: 'V2 user' }, '');
		rows.set(`${PERSISTER_KEY_PREFIX}-${row.queryHash}`, row);

		await expect(persister.read(row.queryHash, undefined)).resolves.toBeUndefined();
		expect(rows.size).toBe(0);
	});

	it('restores a row stamped with the current version', async () => {
		const { rows, persister } = setup();
		const row = storedRow(HomeQueryKeyFactory.home('a'), { sections: [] }, PERSISTED_CACHE_BUSTER);
		rows.set(`${PERSISTER_KEY_PREFIX}-${row.queryHash}`, row);

		await expect(persister.read(row.queryHash, undefined)).resolves.toEqual(row);
	});
});
