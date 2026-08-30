import { writable, get } from 'svelte/store';
import { browser } from '$app/environment';
import { API, PAGE_SOURCE_KEYS } from '$lib/constants';
import { api } from '$lib/api/client';
import { authStore } from '$lib/stores/authStore.svelte';
import { queryClient, setQueryDataWithPersister } from '$lib/queries/QueryClient';
import { getScrobblePreferencesQueryOptions } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQuery.svelte';
import { ScrobblePreferencesQueryKeyFactory } from '$lib/queries/scrobble-preferences/ScrobblePreferencesQueryKeyFactory';
import { registerUserSessionReset } from '$lib/utils/userSessionCleanup';

export type MusicSource = 'listenbrainz' | 'lastfm';
export type MusicSourcePage = keyof typeof PAGE_SOURCE_KEYS;

const CACHED_SOURCE_KEY = 'droppedneedle_primary_source';
export const DEFAULT_SOURCE: MusicSource = 'listenbrainz';

interface MusicSourceState {
	source: MusicSource;
	loaded: boolean;
}

export function isMusicSource(value: unknown): value is MusicSource {
	return value === 'listenbrainz' || value === 'lastfm';
}

// pre-v1.3.0 setPageSource() stored raw strings; PersistedState expects JSON, so
// migrate before any PersistedState constructor reads these keys
export function migratePageSourceKeys(): void {
	if (!browser) return;
	for (const key of Object.values(PAGE_SOURCE_KEYS)) {
		const raw = localStorage.getItem(key);
		if (raw === null) continue;
		if (isMusicSource(raw)) {
			localStorage.setItem(key, JSON.stringify(raw));
		}
	}
}

function readCachedSource(): MusicSource {
	if (!browser) return DEFAULT_SOURCE;
	const stored = localStorage.getItem(CACHED_SOURCE_KEY);
	return isMusicSource(stored) ? stored : DEFAULT_SOURCE;
}

function createMusicSourceStore() {
	const { subscribe, set, update } = writable<MusicSourceState>({
		source: readCachedSource(),
		loaded: false
	});

	let loadPromise: Promise<void> | null = null;
	let mutationVersion = 0;

	function getPageStorageKey(page: MusicSourcePage): string {
		return PAGE_SOURCE_KEYS[page];
	}

	function persistSource(source: MusicSource): void {
		if (browser) {
			localStorage.setItem(CACHED_SOURCE_KEY, source);
		}
	}

	function getCachedSource(): MusicSource {
		return readCachedSource();
	}

	async function load(): Promise<void> {
		const current = get({ subscribe });
		if (current.loaded) return;
		if (loadPromise) return loadPromise;
		const userId = authStore.user?.id;
		if (!userId) {
			update((s) => ({ ...s, loaded: true }));
			return;
		}
		const loadStartedAtVersion = mutationVersion;

		loadPromise = (async () => {
			try {
				if (browser) {
					localStorage.removeItem('home_source');
					localStorage.removeItem('discover_source');
					localStorage.removeItem('artist-discovery_source');
				}
				const data = await queryClient.ensureQueryData(getScrobblePreferencesQueryOptions(userId));
				const fetchedSource = isMusicSource(data.primary_music_source)
					? data.primary_music_source
					: DEFAULT_SOURCE;
				if (mutationVersion === loadStartedAtVersion) {
					persistSource(fetchedSource);
					set({ source: fetchedSource, loaded: true });
				} else {
					update((s) => ({ ...s, loaded: true }));
				}
			} catch {
				update((s) => ({ ...s, loaded: true }));
			} finally {
				loadPromise = null;
			}
		})();

		return loadPromise;
	}

	async function save(source: MusicSource): Promise<boolean> {
		const saveVersion = ++mutationVersion;
		const userId = authStore.user?.id;
		if (!userId) return false;
		try {
			const preferences = await api.global.put<ScrobblePreferences>(API.me.scrobblePreferences(), {
				primary_music_source: source
			});
			if (authStore.user?.id !== userId || mutationVersion !== saveVersion) return true;
			await setQueryDataWithPersister(ScrobblePreferencesQueryKeyFactory.get(userId), preferences);
			persistSource(source);
			set({ source, loaded: true });
			return true;
		} catch {
			return false;
		}
	}

	function setSource(source: MusicSource): void {
		const current = get({ subscribe });
		if (current.loaded && current.source === source) {
			persistSource(source);
			return;
		}
		mutationVersion += 1;
		persistSource(source);
		set({ source, loaded: true });
	}

	function getSource(): MusicSource {
		return get({ subscribe }).source;
	}

	function getPageSource(page: MusicSourcePage): MusicSource {
		const fallbackSource = getSource();
		if (!browser) return fallbackSource;
		const raw = localStorage.getItem(getPageStorageKey(page));
		if (raw === null) return fallbackSource;
		try {
			const parsed: unknown = JSON.parse(raw);
			if (isMusicSource(parsed)) return parsed;
		} catch {
			// fall through to raw-string (old format)
		}
		if (isMusicSource(raw)) return raw;
		return fallbackSource;
	}

	function setPageSource(page: MusicSourcePage, source: MusicSource): void {
		if (!browser) return;
		localStorage.setItem(getPageStorageKey(page), JSON.stringify(source));
	}

	function reset(): void {
		// wipe cached + per-page choices on user switch so a shared browser never
		// carries the previous user's source (AMU-5); next load() re-fetches
		mutationVersion += 1;
		if (browser) {
			localStorage.removeItem(CACHED_SOURCE_KEY);
			for (const key of Object.values(PAGE_SOURCE_KEYS)) localStorage.removeItem(key);
		}
		set({ source: DEFAULT_SOURCE, loaded: false });
	}

	return {
		subscribe,
		load,
		save,
		setSource,
		getSource,
		getCachedSource,
		getPageSource,
		setPageSource,
		reset
	};
}

export const musicSourceStore = createMusicSourceStore();
registerUserSessionReset(() => musicSourceStore.reset());
