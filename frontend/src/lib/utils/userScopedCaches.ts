import { CACHE_KEYS } from '$lib/constants';
import { clearLocalStorageNamespace } from '$lib/utils/localStorageCache';

const REMOTE_LIBRARY_NAMESPACES = [
	CACHE_KEYS.JELLYFIN_SIDEBAR,
	CACHE_KEYS.JELLYFIN_ALBUMS_LIST,
	CACHE_KEYS.NAVIDROME_SIDEBAR,
	CACHE_KEYS.NAVIDROME_ALBUMS_LIST,
	CACHE_KEYS.NAVIDROME_FOLDER_SCOPE,
	CACHE_KEYS.PLEX_SIDEBAR,
	CACHE_KEYS.PLEX_ALBUMS_LIST
];

// Clear user-dependent localStorage caches that TanStack's user-switch reset does not own.
// Album basic/tracks/discovery entries include ownership/request/personalized overlays, so
// they must not survive a logout or account switch even though their provider metadata is reusable.
export function clearUserScopedLocalCaches(): void {
	const failures: unknown[] = [];
	const cleanup = [
		() => clearLocalStorageNamespace(CACHE_KEYS.DISCOVER_QUEUE),
		() => clearLocalStorageNamespace(CACHE_KEYS.TIME_RANGE_OVERVIEW_CACHE),
		() => clearLocalStorageNamespace(CACHE_KEYS.ALBUM_BASIC_CACHE),
		() => clearLocalStorageNamespace(CACHE_KEYS.ALBUM_TRACKS_CACHE),
		() => clearLocalStorageNamespace(CACHE_KEYS.ALBUM_DISCOVERY_CACHE),
		() => clearLocalStorageNamespace(CACHE_KEYS.ALBUM_SOURCE_MATCH_CACHE),
		// Remote library lists that earlier builds kept here; nothing writes them now.
		...REMOTE_LIBRARY_NAMESPACES.map((namespace) => () => clearLocalStorageNamespace(namespace))
	];

	for (const clear of cleanup) {
		try {
			clear();
		} catch (error) {
			failures.push(error);
		}
	}

	if (failures.length === 1) throw failures[0];
	if (failures.length > 1) throw new AggregateError(failures, 'User-scoped cache cleanup failed');
}
