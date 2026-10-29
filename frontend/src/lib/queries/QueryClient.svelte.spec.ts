import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { get, set } from 'idb-keyval';
import { PERSISTER_KEY_PREFIX } from '@tanstack/svelte-query-persist-client';
import { clearPersistedQueryCache } from './IndexedDbPersister.svelte';
import {
	invalidateMusicBrainzProviderQueries,
	invalidateQueriesWithPersister,
	queryClient,
	resetQueryCacheForUserSwitch,
	setQueryDataWithPersister
} from './QueryClient';
import {
	PERSIST,
	PERSIST_PROVIDER,
	PERSISTED_CACHE_BUSTER,
	type StoredQuery
} from './persistedCache';
import { setDownloadScope } from './downloads/downloadScope.svelte';
import { DownloadQueryKeyFactory } from './downloads/DownloadQueryKeyFactory';
import { authStore, type AuthUser } from '$lib/stores/authStore.svelte';
import { ArtistQueryKeyFactory } from './artist/ArtistQueryKeyFactory';
import { LibraryQueryKeyFactory } from './library/LibraryQueryKeyFactory';
import {
	resetMusicBrainzSourceScope,
	setMusicBrainzSourceScope
} from './musicbrainz/sourceScope.svelte';

const keyPrefix = `${PERSISTER_KEY_PREFIX}-`;

function persistedQuery(
	queryKey: readonly unknown[],
	data: unknown,
	provider?: string
): StoredQuery {
	return {
		buster: PERSISTED_CACHE_BUSTER,
		provider,
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

beforeEach(async () => {
	queryClient.clear();
	authStore.clear();
	resetMusicBrainzSourceScope();
	await clearPersistedQueryCache();
});

afterEach(async () => {
	vi.restoreAllMocks();
	queryClient.clear();
	authStore.clear();
	await clearPersistedQueryCache();
});

/** A query that asks to be saved, fetched once so its row is written. */
async function savedQuery(queryKey: readonly unknown[], data: unknown, meta = PERSIST) {
	await queryClient.fetchQuery({ queryKey, meta, queryFn: async () => data });
	const hash = queryClient.getQueryCache().find({ queryKey, exact: true })?.queryHash;
	const storageKey = `${keyPrefix}${hash}`;
	await vi.waitFor(async () => expect(await get(storageKey)).toBeDefined());
	return storageKey;
}

it('removes only provider rows on a source sweep and keeps unrelated or malformed rows', async () => {
	const providerKey = `${keyPrefix}provider-artist`;
	const profileKey = `${keyPrefix}profile`;
	const malformedKey = `${keyPrefix}malformed`;
	const numericKey = 42;
	const unrelatedKey = 'unrelated-application-row';
	const artist = ArtistQueryKeyFactory.basic('artist-1');
	const catalog = LibraryQueryKeyFactory.catalog.stats('u');

	await Promise.all([
		set(providerKey, persistedQuery(artist, { name: 'Artist' }, 'musicbrainz:mirror:a:1')),
		set(profileKey, persistedQuery(catalog, { albums: 1 })),
		set(malformedKey, { malformed: true }),
		set(numericKey, { malformed: true }),
		set(unrelatedKey, { unrelated: true })
	]);

	await invalidateMusicBrainzProviderQueries();

	expect(await get(providerKey)).toBeUndefined();
	expect(await get(profileKey)).toEqual(expect.objectContaining({ queryKey: catalog }));
	expect(await get(malformedKey)).toEqual({ malformed: true });
	expect(await get(numericKey)).toEqual({ malformed: true });
	expect(await get(unrelatedKey)).toEqual({ unrelated: true });
});

it('removes admin task state on demotion and fences late responses through role round trips', async () => {
	setDownloadScope('role-user', 'admin');
	const adminKey = DownloadQueryKeyFactory.tasks('role-user');
	let resolve!: (value: unknown) => void;
	const pending = queryClient
		.fetchQuery({
			queryKey: adminKey,
			staleTime: 0,
			queryFn: () =>
				new Promise((settle) => {
					resolve = settle;
				})
		})
		.catch(() => undefined);
	await vi.waitFor(() => expect(resolve).toBeTypeOf('function'));
	setDownloadScope('role-user', 'user');
	expect(queryClient.getQueryData(adminKey)).toBeUndefined();
	expect(queryClient.getQueryData(DownloadQueryKeyFactory.tasks('role-user'))).toBeUndefined();
	resolve({ items: ['other-user-task'] });
	await pending;
	expect(queryClient.getQueryData(adminKey)).toBeUndefined();
	setDownloadScope('role-user', 'admin');
	expect(DownloadQueryKeyFactory.tasks('role-user')).not.toEqual(adminKey);
	expect(queryClient.getQueryData(DownloadQueryKeyFactory.tasks('role-user'))).toBeUndefined();
	queryClient.clear();
});

it('retains persisted data for ordinary invalidation and removes it for an explicit clear', async () => {
	const key = LibraryQueryKeyFactory.catalog.stats('u');
	const storageKey = await savedQuery(key, { albums: 1 });
	await setQueryDataWithPersister(key, { albums: 2 });
	await invalidateQueriesWithPersister({ queryKey: key });
	expect(await get(storageKey)).toEqual(
		expect.objectContaining({ state: expect.objectContaining({ data: { albums: 2 } }) })
	);
	await invalidateQueriesWithPersister({ queryKey: key }, undefined, { removePersisted: true });
	expect(await get(storageKey)).toBeUndefined();
});

it('clears memory and persisted user data on account switch', async () => {
	const key = LibraryQueryKeyFactory.catalog.stats('old-user');
	const storageKey = await savedQuery(key, { albums: 1 });
	await resetQueryCacheForUserSwitch();
	expect(queryClient.getQueryData(key)).toBeUndefined();
	expect(await get(storageKey)).toBeUndefined();
});

it('continues selective cleanup and invalidates memory after an individual deletion fails', async () => {
	const retained = `${keyPrefix}a-failed`;
	const removed = `${keyPrefix}b-removed`;
	const key = ArtistQueryKeyFactory.basic('fixture');
	await queryClient.fetchQuery({
		queryKey: key,
		meta: PERSIST_PROVIDER,
		queryFn: async () => ({ name: 'Artist' })
	});
	await set(retained, persistedQuery(key, { name: 'Artist' }, 'musicbrainz:a'));
	await set(removed, persistedQuery(ArtistQueryKeyFactory.basic('second'), {}, 'musicbrainz:a'));
	const failure = new DOMException('Fixture deletion failure', 'UnknownError');
	const originalDelete = IDBObjectStore.prototype.delete;
	vi.spyOn(IDBObjectStore.prototype, 'delete').mockImplementation(function (
		this: IDBObjectStore,
		item
	) {
		if (item === retained) throw failure;
		return originalDelete.call(this, item);
	});
	await expect(invalidateMusicBrainzProviderQueries()).rejects.toBe(failure);
	expect(await get(retained)).toEqual(expect.objectContaining({ queryKey: key }));
	expect(await get(removed)).toBeUndefined();
	expect(queryClient.getQueryState(key)?.isInvalidated).toBe(true);
});

it('drops answers from before a source switch, in memory and in the browser copy', async () => {
	const user: AuthUser = {
		id: 'source-user',
		role: 'admin',
		display_name: 'Source user',
		username: 'source-user',
		username_display: 'source-user',
		email: 'source@example.test',
		avatar_url: null,
		providers: []
	};
	authStore.setUser(user);
	setMusicBrainzSourceScope({ source_mode: 'mirror', source_id: 'a-first', generation: 1 });
	const key = ArtistQueryKeyFactory.basic('artist-1');
	const storageKey = await savedQuery(key, { name: 'Old source' }, PERSIST_PROVIDER);
	let settle!: (value: { name: string }) => void;
	const pending = queryClient
		.fetchQuery({
			queryKey: key,
			meta: PERSIST_PROVIDER,
			staleTime: 0,
			queryFn: () =>
				new Promise<{ name: string }>((resolve) => {
					settle = resolve;
				})
		})
		.catch(() => undefined);
	await vi.waitFor(() => expect(settle).toBeTypeOf('function'));

	setMusicBrainzSourceScope({ source_mode: 'mirror', source_id: 'b', generation: 2 });
	expect(queryClient.getQueryData(key)).toBeUndefined();
	settle({ name: 'Late old source' });
	await pending;
	expect(queryClient.getQueryData(key)).toBeUndefined();
	await vi.waitFor(async () => expect(await get(storageKey)).toBeUndefined());

	const fresh = await queryClient.fetchQuery({
		queryKey: key,
		meta: PERSIST_PROVIDER,
		queryFn: async () => ({ name: 'Current source' })
	});
	expect(fresh).toEqual({ name: 'Current source' });
});
