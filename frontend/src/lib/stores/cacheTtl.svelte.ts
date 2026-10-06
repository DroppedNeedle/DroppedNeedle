import { browser } from '$app/environment';
import { CACHE_TTL } from '$lib/constants';
import { api } from '$lib/api/client';
import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
import { updateHomeCacheTTL } from '$lib/utils/homeCache';
import { updateDiscoveryCacheTTL } from '$lib/stores/discoveryCache';
import { updateDiscoverQueueCacheTTL } from '$lib/utils/discoverQueueCache';
import { updateSearchCacheTTL } from '$lib/stores/search';
import { updateJellyfinSidebarCacheTTL } from '$lib/utils/jellyfinLibraryCache';
import {
	updatePlexSidebarCacheTTL,
	updatePlexAlbumsListCacheTTL
} from '$lib/utils/plexLibraryCache';
import { updateLocalFilesSidebarCacheTTL } from '$lib/utils/localFilesCache';

export interface CacheTTLs {
	home: number;
	discover: number;
	library: number;
	recentlyAdded: number;
	discoverQueue: number;
	search: number;
	localFilesSidebar: number;
	jellyfinSidebar: number;
	plexSidebar: number;
	playlistSources: number;
	discoverQueuePollingInterval: number;
	discoverQueueAutoGenerate: boolean;
}

const DEFAULTS: CacheTTLs = {
	home: CACHE_TTL.HOME,
	discover: CACHE_TTL.DISCOVER,
	library: CACHE_TTL.LIBRARY,
	recentlyAdded: CACHE_TTL.RECENTLY_ADDED,
	discoverQueue: CACHE_TTL.DISCOVER_QUEUE,
	search: CACHE_TTL.SEARCH,
	localFilesSidebar: CACHE_TTL.LOCAL_FILES_SIDEBAR,
	jellyfinSidebar: CACHE_TTL.JELLYFIN_SIDEBAR,
	plexSidebar: CACHE_TTL.PLEX_SIDEBAR,
	playlistSources: CACHE_TTL.PLAYLIST_SOURCES,
	discoverQueuePollingInterval: 4000,
	discoverQueueAutoGenerate: true
};

// ST6 dual-TTL unification: `$state` so TanStack option closures that read it
// re-evaluate when initCacheTTLs() resolves (or a later settings save lands).
let resolved = $state<CacheTTLs>({ ...DEFAULTS });
let initialized = false;

/**
 * Reactive TTL lookup for query options (core group: home, discover satellites,
 * library-native lists, recently-added, search). Semantics: a settings save
 * affects freshness decisions from then on - it never retro-refetches already
 * fresh data; queries simply consult the new window at their next mount /
 * focus / staleness check.
 *
 * Until initCacheTTLs() resolves (deferred post-mount at AuthenticatedAppShell),
 * the caller's static CACHE_TTL constant wins, keeping pre-init behavior
 * byte-identical to the shipped defaults; after a failed fetch the store holds
 * those same defaults. The fallback argument must be the consuming query's
 * shipped constant so pre-init and default states agree.
 */
export function ttl<K extends keyof CacheTTLs>(key: K, fallback?: CacheTTLs[K]): CacheTTLs[K] {
	// Read through the $state proxy even on the fallback path so closures stay
	// subscribed and re-run once initialization replaces the values.
	const current = resolved[key];
	return initialized ? current : (fallback ?? current);
}

function applyTTLs(ttls: CacheTTLs): void {
	updateHomeCacheTTL(ttls.home);
	updateDiscoveryCacheTTL(ttls.discover);
	updateDiscoverQueueCacheTTL(ttls.discoverQueue);
	updateSearchCacheTTL(ttls.search);
	updateLocalFilesSidebarCacheTTL(ttls.localFilesSidebar);
	updateJellyfinSidebarCacheTTL(ttls.jellyfinSidebar);
	updatePlexSidebarCacheTTL(ttls.plexSidebar);
	updatePlexAlbumsListCacheTTL(ttls.plexSidebar);
}

export async function initCacheTTLs(): Promise<void> {
	if (!browser || initialized) return;
	initialized = true;

	try {
		const data = await api.global.v3.GET(SETTINGS_ENDPOINTS.cacheTtls());
		resolved = {
			home: data.home ?? DEFAULTS.home,
			discover: data.discover ?? DEFAULTS.discover,
			library: data.library ?? DEFAULTS.library,
			recentlyAdded: data.recently_added ?? DEFAULTS.recentlyAdded,
			discoverQueue: data.discover_queue ?? DEFAULTS.discoverQueue,
			search: data.search ?? DEFAULTS.search,
			localFilesSidebar: data.local_files_sidebar ?? DEFAULTS.localFilesSidebar,
			jellyfinSidebar: data.jellyfin_sidebar ?? DEFAULTS.jellyfinSidebar,
			plexSidebar: data.plex_sidebar ?? DEFAULTS.plexSidebar,
			playlistSources: data.playlist_sources ?? DEFAULTS.playlistSources,
			discoverQueuePollingInterval:
				data.discover_queue_polling_interval ?? DEFAULTS.discoverQueuePollingInterval,
			discoverQueueAutoGenerate:
				data.discover_queue_auto_generate ?? DEFAULTS.discoverQueueAutoGenerate
		};
		applyTTLs(resolved);
	} catch {
		resolved = { ...DEFAULTS };
		applyTTLs(resolved);
	}
}

export function getCacheTTLs(): CacheTTLs {
	return resolved;
}

export function getCacheTTL<K extends keyof CacheTTLs>(key: K): CacheTTLs[K] {
	return resolved[key];
}
