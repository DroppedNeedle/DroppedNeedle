import { CACHE_KEYS } from '$lib/constants';
import { clearLocalStorageNamespace } from '$lib/utils/localStorageCache';
import { clearNavidromeLocalCaches } from '$lib/utils/navidromeLibraryCache';

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
		clearNavidromeLocalCaches
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
