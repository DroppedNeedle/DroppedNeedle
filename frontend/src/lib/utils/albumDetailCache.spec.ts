import { beforeEach, expect, it, vi } from 'vitest';

vi.mock('$app/environment', () => ({ browser: true }));

const removalFailures = new Set<string>();
const memoryStorage = {
	data: new Map<string, string>(),
	get length() {
		return this.data.size;
	},
	clear() {
		this.data.clear();
	},
	getItem(key: string) {
		return this.data.get(key) ?? null;
	},
	key(index: number) {
		return [...this.data.keys()][index] ?? null;
	},
	removeItem(key: string) {
		if (removalFailures.has(key)) throw new Error('localStorage removal failed');
		this.data.delete(key);
	},
	setItem(key: string, value: string) {
		this.data.set(key, value);
	}
};

vi.stubGlobal('localStorage', memoryStorage);

import { CACHE_KEYS } from '$lib/constants';
import { clearMusicBrainzProviderCaches } from './albumDetailCache';

const providerNamespaces = [
	CACHE_KEYS.ALBUM_BASIC_CACHE,
	CACHE_KEYS.ALBUM_TRACKS_CACHE,
	CACHE_KEYS.ALBUM_DISCOVERY_CACHE
] as const;

const unrelatedNamespaces = [
	CACHE_KEYS.ALBUM_LASTFM_CACHE,
	CACHE_KEYS.ALBUM_YOUTUBE_CACHE,
	CACHE_KEYS.ALBUM_SOURCE_MATCH_CACHE,
	CACHE_KEYS.SEARCH
] as const;

beforeEach(() => {
	memoryStorage.clear();
	removalFailures.clear();
});

it('clears only the proven MusicBrainz album cache namespaces', () => {
	for (const namespace of [...providerNamespaces, ...unrelatedNamespaces]) {
		const payload = JSON.stringify({ data: { namespace }, timestamp: Date.now() });
		memoryStorage.setItem(namespace, payload);
		memoryStorage.setItem(`${namespace}_album-1`, payload);
	}

	expect(clearMusicBrainzProviderCaches()).toBe(true);

	for (const namespace of providerNamespaces) {
		expect(memoryStorage.getItem(namespace)).toBeNull();
		expect(memoryStorage.getItem(`${namespace}_album-1`)).toBeNull();
	}
	for (const namespace of unrelatedNamespaces) {
		expect(memoryStorage.getItem(namespace)).not.toBeNull();
		expect(memoryStorage.getItem(`${namespace}_album-1`)).not.toBeNull();
	}
});

it('attempts every provider namespace and retries failed removals without exposing errors', () => {
	for (const namespace of providerNamespaces) {
		const payload = JSON.stringify({ data: { namespace }, timestamp: Date.now() });
		memoryStorage.setItem(namespace, payload);
		memoryStorage.setItem(`${namespace}_album-1`, payload);
	}
	removalFailures.add(`${providerNamespaces[0]}_album-1`);
	removalFailures.add(providerNamespaces[1]);

	expect(clearMusicBrainzProviderCaches()).toBe(false);
	expect(memoryStorage.getItem(providerNamespaces[0])).toBeNull();
	expect(memoryStorage.getItem(`${providerNamespaces[0]}_album-1`)).not.toBeNull();
	expect(memoryStorage.getItem(providerNamespaces[1])).not.toBeNull();
	expect(memoryStorage.getItem(`${providerNamespaces[1]}_album-1`)).not.toBeNull();
	expect(memoryStorage.getItem(providerNamespaces[2])).toBeNull();
	expect(memoryStorage.getItem(`${providerNamespaces[2]}_album-1`)).toBeNull();

	removalFailures.clear();
	expect(clearMusicBrainzProviderCaches()).toBe(true);
	for (const namespace of providerNamespaces) {
		expect(memoryStorage.getItem(namespace)).toBeNull();
		expect(memoryStorage.getItem(`${namespace}_album-1`)).toBeNull();
	}
});
