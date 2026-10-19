import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
import type { PlexAlbumSummary, PlexLibraryStats } from '$lib/types';
import { createLocalStorageCache } from '$lib/utils/localStorageCache';

type PlexSidebarData = {
	recentAlbums: PlexAlbumSummary[];
	genres: string[];
	moods: string[];
	stats: PlexLibraryStats | null;
};

type PlexAlbumsListData = {
	items: PlexAlbumSummary[];
	total: number;
};

const plexSidebarCache = createLocalStorageCache<PlexSidebarData>(
	CACHE_KEYS.PLEX_SIDEBAR,
	CACHE_TTL.PLEX_SIDEBAR
);

const plexAlbumsListCache = createLocalStorageCache<PlexAlbumsListData>(
	CACHE_KEYS.PLEX_ALBUMS_LIST,
	CACHE_TTL.PLEX_ALBUMS_LIST,
	{ maxEntries: 80 }
);

export const getPlexSidebarCachedData = plexSidebarCache.get;
export const setPlexSidebarCachedData = plexSidebarCache.set;
export const isPlexSidebarCacheStale = plexSidebarCache.isStale;

export const getPlexAlbumsListCachedData = plexAlbumsListCache.get;
export const setPlexAlbumsListCachedData = plexAlbumsListCache.set;
export const isPlexAlbumsListCacheStale = plexAlbumsListCache.isStale;
