import { beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient } from '@tanstack/svelte-query';
import { CACHE_KEYS } from '$lib/constants';
import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';
import { setQueueCachedData } from '$lib/utils/discoverQueueCache';
import { overviewCacheSuffix } from '$lib/utils/timeRangeCache';
import { clearUserScopedLocalCaches } from '$lib/utils/userScopedCaches';

// A switch must drop both the user-keyed TanStack home cache and the localStorage caches the
// query-cache reset misses; asserts the two mechanisms in isolation, not the logout() orchestration.
describe('clear-on-user-switch (AMU-5)', () => {
	beforeEach(() => {
		localStorage.clear();
	});

	it('queryClient.clear() drops the user-keyed home entry', () => {
		const qc = new QueryClient();
		const key = HomeQueryKeyFactory.home('user-a');
		qc.setQueryData(key, { greeting: 'hi A' });
		expect(qc.getQueryData(key)).toBeDefined();

		qc.clear();
		expect(qc.getQueryData(key)).toBeUndefined();
	});

	it('clearUserScopedLocalCaches() removes the prior user discover-queue + time-range entries', () => {
		setQueueCachedData({ items: [], currentIndex: 0, queueId: 'q-a' }, 'user-a');
		const queueKey = `${CACHE_KEYS.DISCOVER_QUEUE}_user-a`;
		const trKey = `${CACHE_KEYS.TIME_RANGE_OVERVIEW_CACHE}_${overviewCacheSuffix(
			'user-a',
			'album',
			'listenbrainz',
			'/api/v1/home/your-top/albums'
		)}`;
		localStorage.setItem(trKey, JSON.stringify({ data: {}, timestamp: Date.now() }));

		expect(localStorage.getItem(queueKey)).not.toBeNull();
		expect(localStorage.getItem(trKey)).not.toBeNull();

		clearUserScopedLocalCaches();

		expect(localStorage.getItem(queueKey)).toBeNull();
		expect(localStorage.getItem(trKey)).toBeNull();
	});

	it('clears album ownership caches for both users at the sanctioned user-switch boundary', () => {
		const albumIds = [
			{ id: 'album-user-a', overlay: 'user-a' },
			{ id: 'album-user-b', overlay: 'user-b' }
		];
		const musicbrainzNamespaces = [
			CACHE_KEYS.ALBUM_BASIC_CACHE,
			CACHE_KEYS.ALBUM_TRACKS_CACHE,
			CACHE_KEYS.ALBUM_DISCOVERY_CACHE
		] as const;
		const unrelatedNamespaces = [
			CACHE_KEYS.ALBUM_LASTFM_CACHE,
			CACHE_KEYS.ALBUM_YOUTUBE_CACHE
		] as const;

		for (const album of albumIds) {
			const payload = JSON.stringify({ data: { overlay: album.overlay }, timestamp: Date.now() });
			for (const namespace of [...musicbrainzNamespaces, ...unrelatedNamespaces]) {
				localStorage.setItem(`${namespace}_${album.id}`, payload);
			}
		}

		clearUserScopedLocalCaches();

		for (const album of albumIds) {
			for (const namespace of musicbrainzNamespaces) {
				expect(localStorage.getItem(`${namespace}_${album.id}`)).toBeNull();
			}
			for (const namespace of unrelatedNamespaces) {
				expect(localStorage.getItem(`${namespace}_${album.id}`)).not.toBeNull();
			}
		}
	});

	it('attempts every user namespace and Navidrome helper after first and middle removal failures', () => {
		const userNamespaces = [
			CACHE_KEYS.DISCOVER_QUEUE,
			CACHE_KEYS.TIME_RANGE_OVERVIEW_CACHE,
			CACHE_KEYS.ALBUM_BASIC_CACHE,
			CACHE_KEYS.ALBUM_TRACKS_CACHE,
			CACHE_KEYS.ALBUM_DISCOVERY_CACHE
		] as const;
		const navidromeNamespaces = [
			CACHE_KEYS.NAVIDROME_SIDEBAR,
			CACHE_KEYS.NAVIDROME_ALBUMS_LIST,
			CACHE_KEYS.NAVIDROME_FOLDER_SCOPE,
			CACHE_KEYS.ALBUM_SOURCE_MATCH_CACHE
		] as const;
		const namespaces = [...userNamespaces, ...navidromeNamespaces];
		const seededKeys = namespaces;
		const failedKeys = new Set([userNamespaces[0], userNamespaces[3]]);
		for (const key of seededKeys) localStorage.setItem(key, 'cached');

		const attemptedKeys = new Set<string>();
		const originalRemoveItem = Storage.prototype.removeItem;
		const removeItem = vi.spyOn(Storage.prototype, 'removeItem').mockImplementation(function (key) {
			attemptedKeys.add(key);
			if (failedKeys.has(key)) throw new Error(`remove failed for ${key}`);
			return originalRemoveItem.call(this, key);
		});

		try {
			expect(() => clearUserScopedLocalCaches()).toThrow(AggregateError);
		} finally {
			removeItem.mockRestore();
		}

		expect(attemptedKeys).toEqual(new Set(seededKeys));
		expect(localStorage.getItem(userNamespaces[0])).not.toBeNull();
		expect(localStorage.getItem(userNamespaces[3])).not.toBeNull();
		for (const key of seededKeys.filter((key) => !failedKeys.has(key))) {
			expect(localStorage.getItem(key)).toBeNull();
		}
	});
});
