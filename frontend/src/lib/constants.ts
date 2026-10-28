export const AUTH_FREE_PATHS = ['/login', '/setup', '/auth/callback', '/recover-password'];

// concert distances are stored in km but displayed in miles
export const KM_PER_MILE = 1.609;

const CACHE_KEY_GROUPS = {
	core: {
		RECENTLY_ADDED: 'droppedneedle_recently_added',
		HOME_CACHE: 'droppedneedle_home_cache',
		DISCOVER_QUEUE: 'droppedneedle_discover_deck',
		SEARCH: 'droppedneedle_search_cache'
	},
	library: {
		LOCAL_FILES_SIDEBAR: 'droppedneedle_local_files_sidebar',
		JELLYFIN_SIDEBAR: 'droppedneedle_jellyfin_sidebar',
		JELLYFIN_ALBUMS_LIST: 'droppedneedle_jellyfin_albums_list',
		NAVIDROME_SIDEBAR: 'droppedneedle_navidrome_sidebar',
		NAVIDROME_ALBUMS_LIST: 'droppedneedle_navidrome_albums_list',
		NAVIDROME_FOLDER_SCOPE: 'droppedneedle_navidrome_folder_scope',
		PLEX_SIDEBAR: 'droppedneedle_plex_sidebar',
		PLEX_ALBUMS_LIST: 'droppedneedle_plex_albums_list',
		LOCAL_FILES_ALBUMS_LIST: 'droppedneedle_local_files_albums_list'
	},
	detail: {
		ARTIST_BASIC_CACHE: 'droppedneedle_artist_basic_cache',
		ARTIST_EXTENDED_CACHE: 'droppedneedle_artist_extended_cache',
		ARTIST_LASTFM_CACHE: 'droppedneedle_artist_lastfm_cache'
	},
	charts: {
		TIME_RANGE_OVERVIEW_CACHE: 'droppedneedle_time_range_overview_cache',
		GENRE_DETAIL_CACHE: 'droppedneedle_genre_detail_cache'
	}
} as const;

export const CACHE_KEYS = {
	...CACHE_KEY_GROUPS.core,
	...CACHE_KEY_GROUPS.library,
	...CACHE_KEY_GROUPS.detail,
	...CACHE_KEY_GROUPS.charts
} as const;

export const PAGE_SOURCE_KEYS = {
	home: 'droppedneedle_source_home',
	discover: 'droppedneedle_source_discover',
	artist: 'droppedneedle_source_artist',
	trending: 'droppedneedle_source_trending',
	popular: 'droppedneedle_source_popular',
	yourTop: 'droppedneedle_source_your_top'
} as const;

const CACHE_TTL_GROUPS = {
	core: {
		DEFAULT: 5 * 60 * 1000,
		LIBRARY: 5 * 60 * 1000,
		LIBRARY_NATIVE: 60 * 1000,
		RECENTLY_ADDED: 5 * 60 * 1000,
		HOME: 5 * 60 * 1000,
		DISCOVER: 30 * 60 * 1000,
		DISCOVER_QUEUE: 24 * 60 * 60 * 1000,
		SEARCH: 5 * 60 * 1000,
		LYRICS: 60 * 60 * 1000
	},
	library: {
		LOCAL_FILES_SIDEBAR: 2 * 60 * 1000,
		LOCAL_FILES_ALBUMS_LIST: 2 * 60 * 1000,
		PLAYLIST_SOURCES: 15 * 60 * 1000
	},
	detail: {
		ALBUM_DETAIL_BASIC: 5 * 60 * 1000,
		ALBUM_DETAIL_TRACKS: 15 * 60 * 1000,
		ALBUM_DETAIL_DISCOVERY: 30 * 60 * 1000,
		ALBUM_DETAIL_LASTFM: 30 * 60 * 1000,
		ALBUM_DETAIL_YOUTUBE: 60 * 60 * 1000,
		ALBUM_DETAIL_SOURCE_MATCH: 5 * 60 * 1000,
		ALBUM_DETAIL_EDITIONS: 5 * 60 * 1000,
		ARTIST_DETAIL_BASIC: 5 * 60 * 1000,
		ARTIST_DETAIL_EXTENDED: 30 * 60 * 1000,
		ARTIST_DETAIL_LASTFM: 30 * 60 * 1000,
		ARTIST_DISCOVERY: 5 * 60 * 1000
	},
	charts: {
		TIME_RANGE_OVERVIEW: 2 * 60 * 1000,
		GENRE_DETAIL: 5 * 60 * 1000
	},
	version: {
		VERSION_INFO: 60 * 60 * 1000,
		UPDATE_CHECK: 30 * 60 * 1000,
		RELEASE_HISTORY: 60 * 60 * 1000
	}
} as const;

export const CACHE_TTL = {
	...CACHE_TTL_GROUPS.core,
	...CACHE_TTL_GROUPS.library,
	...CACHE_TTL_GROUPS.detail,
	...CACHE_TTL_GROUPS.charts,
	...CACHE_TTL_GROUPS.version
} as const;

export const API_SIZES = {
	XS: 250,
	SM: 250,
	MD: 250,
	LG: 500,
	XL: 500,
	HERO: 500,
	FULL: 500
} as const;

export const TOAST_DURATION = 2000;

export const STATUS_COLORS = {
	REQUESTED: '#F59E0B',
	MONITORED: '#6B7280'
} as const;

export const YOUTUBE_PLAYER_ELEMENT_ID = 'yt-player-embed';
