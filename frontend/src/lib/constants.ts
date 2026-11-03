// The page a server started on v2 data serves in place of the app.
export const UPGRADE_PATH = '/upgrade';

export const AUTH_FREE_PATHS = [
	'/login',
	'/setup',
	'/auth/callback',
	'/recover-password',
	UPGRADE_PATH
];

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
	SMALL: 250,
	LARGE: 500
} as const;

export const TOAST_DURATION = 2000;

export const YOUTUBE_PLAYER_ELEMENT_ID = 'yt-player-embed';
