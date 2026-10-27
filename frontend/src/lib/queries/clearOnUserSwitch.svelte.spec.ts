import { beforeEach, describe, expect, it, vi } from 'vitest';
import { QueryClient } from '@tanstack/svelte-query';
import { CACHE_KEYS } from '$lib/constants';
import {
	resetMusicBrainzSourceScope,
	setMusicBrainzSourceScope
} from '$lib/queries/musicbrainz/sourceScope.svelte';
import { HomeQueryKeyFactory } from './HomeQueryKeyFactory';
import { setQueueCachedData } from '$lib/utils/discoverQueueCache';
import { clearUserScopedLocalCaches } from '$lib/utils/userScopedCaches';

// A switch must drop both the user-keyed TanStack home cache and the localStorage caches the
// query-cache reset misses; asserts the two mechanisms in isolation, not the logout() orchestration.
describe('clear-on-user-switch', () => {
	beforeEach(() => {
		localStorage.clear();
		resetMusicBrainzSourceScope();
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
		// The queue writer drops data without a source scope; establish one so
		// the seeded entry exists to be cleared.
		setMusicBrainzSourceScope(
			{ source_mode: 'mirror', source_id: 'mirror-a', generation: 7 },
			'user-a'
		);
		setQueueCachedData({ items: [], currentIndex: 0, queueId: 'q-a' }, 'user-a');
		const queueKey = `${CACHE_KEYS.DISCOVER_QUEUE}_user-a`;
		const trKey = `${CACHE_KEYS.TIME_RANGE_OVERVIEW_CACHE}_user-a:album:listenbrainz`;
		localStorage.setItem(trKey, JSON.stringify({ data: {}, timestamp: Date.now() }));

		expect(localStorage.getItem(queueKey)).not.toBeNull();
		expect(localStorage.getItem(trKey)).not.toBeNull();

		clearUserScopedLocalCaches();

		expect(localStorage.getItem(queueKey)).toBeNull();
		expect(localStorage.getItem(trKey)).toBeNull();
	});

	it('attempts every user and remote library namespace after first and middle removal failures', () => {
		const userNamespaces = [
			CACHE_KEYS.DISCOVER_QUEUE,
			CACHE_KEYS.TIME_RANGE_OVERVIEW_CACHE
		] as const;
		const remoteNamespaces = [
			CACHE_KEYS.JELLYFIN_SIDEBAR,
			CACHE_KEYS.JELLYFIN_ALBUMS_LIST,
			CACHE_KEYS.NAVIDROME_SIDEBAR,
			CACHE_KEYS.NAVIDROME_ALBUMS_LIST,
			CACHE_KEYS.NAVIDROME_FOLDER_SCOPE,
			CACHE_KEYS.PLEX_SIDEBAR,
			CACHE_KEYS.PLEX_ALBUMS_LIST
		] as const;
		const namespaces = [...userNamespaces, ...remoteNamespaces];
		const seededKeys = namespaces;
		const failedKeys: Set<string> = new Set([userNamespaces[0], remoteNamespaces[2]]);
		for (const key of seededKeys) localStorage.setItem(key, 'cached');

		const attemptedKeys = new Set<string>();
		const originalRemoveItem = Storage.prototype.removeItem;
		const removeItem = vi.spyOn(Storage.prototype, 'removeItem').mockImplementation((key) => {
			attemptedKeys.add(key);
			if (failedKeys.has(key)) throw new Error(`remove failed for ${key}`);
			return originalRemoveItem.call(localStorage, key);
		});

		try {
			expect(() => clearUserScopedLocalCaches()).toThrow(AggregateError);
		} finally {
			removeItem.mockRestore();
		}

		expect(attemptedKeys).toEqual(new Set(seededKeys));
		expect(localStorage.getItem(userNamespaces[0])).not.toBeNull();
		expect(localStorage.getItem(remoteNamespaces[2])).not.toBeNull();
		for (const key of seededKeys.filter((key) => !failedKeys.has(key))) {
			expect(localStorage.getItem(key)).toBeNull();
		}
	});
});
