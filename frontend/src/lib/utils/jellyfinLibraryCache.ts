import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
import type { JellyfinAlbumSummary, JellyfinLibraryStats } from '$lib/types';
import { createLocalStorageCache } from '$lib/utils/localStorageCache';

type JellyfinSidebarData = {
	recentAlbums: JellyfinAlbumSummary[];
	favoriteAlbums: JellyfinAlbumSummary[];
	genres: string[];
	stats: JellyfinLibraryStats | null;
};

type JellyfinAlbumsListData = {
	items: JellyfinAlbumSummary[];
	total: number;
};

const jellyfinSidebarCache = createLocalStorageCache<JellyfinSidebarData>(
	CACHE_KEYS.JELLYFIN_SIDEBAR,
	CACHE_TTL.JELLYFIN_SIDEBAR
);

const jellyfinAlbumsListCache = createLocalStorageCache<JellyfinAlbumsListData>(
	CACHE_KEYS.JELLYFIN_ALBUMS_LIST,
	CACHE_TTL.JELLYFIN_ALBUMS_LIST,
	{ maxEntries: 80 }
);

export const getJellyfinSidebarCachedData = jellyfinSidebarCache.get;
export const setJellyfinSidebarCachedData = jellyfinSidebarCache.set;
export const isJellyfinSidebarCacheStale = jellyfinSidebarCache.isStale;

export const getJellyfinAlbumsListCachedData = jellyfinAlbumsListCache.get;
export const setJellyfinAlbumsListCachedData = jellyfinAlbumsListCache.set;
export const isJellyfinAlbumsListCacheStale = jellyfinAlbumsListCache.isStale;
