import { browser } from '$app/environment';
import { CACHE_TTL } from '$lib/constants';
import { api } from '$lib/api/client';
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
import { recentlyAddedStore } from '$lib/stores/recentlyAdded';

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

let resolved: CacheTTLs = { ...DEFAULTS };
let initialized = false;

function applyTTLs(ttls: CacheTTLs): void {
	updateHomeCacheTTL(ttls.home);
	recentlyAddedStore.updateCacheTTL(ttls.recentlyAdded);
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
		const data = await api.global.get<Record<string, unknown>>('/api/v1/settings/cache-ttls');
		resolved = {
			home: (data.home as number) ?? DEFAULTS.home,
			discover: (data.discover as number) ?? DEFAULTS.discover,
			library: (data.library as number) ?? DEFAULTS.library,
			recentlyAdded: (data.recently_added as number) ?? DEFAULTS.recentlyAdded,
			discoverQueue: (data.discover_queue as number) ?? DEFAULTS.discoverQueue,
			search: (data.search as number) ?? DEFAULTS.search,
			localFilesSidebar: (data.local_files_sidebar as number) ?? DEFAULTS.localFilesSidebar,
			jellyfinSidebar: (data.jellyfin_sidebar as number) ?? DEFAULTS.jellyfinSidebar,
			plexSidebar: (data.plex_sidebar as number) ?? DEFAULTS.plexSidebar,
			playlistSources: (data.playlist_sources as number) ?? DEFAULTS.playlistSources,
			discoverQueuePollingInterval:
				(data.discover_queue_polling_interval as number) ?? DEFAULTS.discoverQueuePollingInterval,
			discoverQueueAutoGenerate:
				(data.discover_queue_auto_generate as boolean) ?? DEFAULTS.discoverQueueAutoGenerate
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
