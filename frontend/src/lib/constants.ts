export const AUTH_FREE_PATHS = ['/login', '/setup', '/auth/callback', '/recover-password'];

// concert distances are stored in km but displayed in miles
export const KM_PER_MILE = 1.609;

export const PAGE_SOURCE_KEYS = {
	home: 'droppedneedle_source_home',
	discover: 'droppedneedle_source_discover',
	artist: 'droppedneedle_source_artist',
	trending: 'droppedneedle_source_trending',
	popular: 'droppedneedle_source_popular',
	yourTop: 'droppedneedle_source_your_top'
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
