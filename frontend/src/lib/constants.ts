import type { MusicSource } from './stores/musicSource';

export const AUTH_FREE_PATHS = ['/login', '/setup', '/auth/callback'];

// concert distances are stored in km but displayed in miles (owner decision U6)
export const KM_PER_MILE = 1.609;

const CACHE_KEY_GROUPS = {
	core: {
		LIBRARY_MBIDS: 'droppedneedle_library_mbids',
		RECENTLY_ADDED: 'droppedneedle_recently_added',
		HOME_CACHE: 'droppedneedle_home_cache',
		DISCOVER_QUEUE: 'droppedneedle_discover_queue',
		SEARCH: 'droppedneedle_search_cache'
	},
	library: {
		LOCAL_FILES_SIDEBAR: 'droppedneedle_local_files_sidebar',
		JELLYFIN_SIDEBAR: 'droppedneedle_jellyfin_sidebar',
		JELLYFIN_ALBUMS_LIST: 'droppedneedle_jellyfin_albums_list',
		NAVIDROME_SIDEBAR: 'droppedneedle_navidrome_sidebar',
		NAVIDROME_ALBUMS_LIST: 'droppedneedle_navidrome_albums_list',
		PLEX_SIDEBAR: 'droppedneedle_plex_sidebar',
		PLEX_ALBUMS_LIST: 'droppedneedle_plex_albums_list',
		LOCAL_FILES_ALBUMS_LIST: 'droppedneedle_local_files_albums_list'
	},
	detail: {
		ALBUM_BASIC_CACHE: 'droppedneedle_album_basic_cache',
		ALBUM_TRACKS_CACHE: 'droppedneedle_album_tracks_cache',
		ALBUM_DISCOVERY_CACHE: 'droppedneedle_album_discovery_cache',
		ALBUM_LASTFM_CACHE: 'droppedneedle_album_lastfm_cache',
		ALBUM_YOUTUBE_CACHE: 'droppedneedle_album_youtube_cache',
		ALBUM_SOURCE_MATCH_CACHE: 'droppedneedle_album_source_match_cache',
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
		SCAN_STATUS: 2 * 1000,
		SCAN_STATUS_IDLE: 15 * 1000,
		RECENTLY_ADDED: 5 * 60 * 1000,
		HOME: 5 * 60 * 1000,
		DISCOVER: 30 * 60 * 1000,
		DISCOVER_QUEUE: 24 * 60 * 60 * 1000,
		SEARCH: 5 * 60 * 1000,
		LYRICS: 60 * 60 * 1000
	},
	library: {
		LOCAL_FILES_SIDEBAR: 2 * 60 * 1000,
		JELLYFIN_SIDEBAR: 2 * 60 * 1000,
		JELLYFIN_ALBUMS_LIST: 2 * 60 * 1000,
		NAVIDROME_SIDEBAR: 2 * 60 * 1000,
		NAVIDROME_ALBUMS_LIST: 2 * 60 * 1000,
		PLEX_SIDEBAR: 2 * 60 * 1000,
		PLEX_ALBUMS_LIST: 2 * 60 * 1000,
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

export const API = {
	artist: {
		basic: (id: string) => `/api/v1/artists/${id}`,
		extended: (id: string) => `/api/v1/artists/${id}/extended`,
		releases: (id: string, offset: number, limit: number) =>
			`/api/v1/artists/${id}/releases?offset=${offset}&limit=${limit}`,
		similarArtists: (id: string, source: MusicSource, count: number = 15) =>
			`/api/v1/artists/${id}/similar?count=${count}&source=${source}`,
		topSongs: (id: string, source: MusicSource, count: number = 10) =>
			`/api/v1/artists/${id}/top-songs?count=${count}&source=${source}`,
		topAlbums: (id: string, source: MusicSource, count: number = 10) =>
			`/api/v1/artists/${id}/top-albums?count=${count}&source=${source}`,
		lastFmEnrichment: (id: string, artistName: string) => {
			const params = new URLSearchParams({ artist_name: artistName });
			return `/api/v1/artists/${id}/lastfm?${params.toString()}`;
		},
		follow: (id: string) => `/api/v1/artists/${id}/follow`,
		autoDownload: (id: string) => `/api/v1/artists/${id}/auto-download`,
		purchaseOptions: (id: string, artistName: string) => {
			const params = new URLSearchParams({ name: artistName });
			return `/api/v1/artists/${id}/purchase-options?${params.toString()}`;
		}
	},
	following: {
		artists: () => '/api/v1/following/artists',
		newReleases: (limit: number, offset: number) =>
			`/api/v1/following/new-releases?limit=${limit}&offset=${offset}`,
		recentReleases: (days: number, limit: number, includeOwned = true) =>
			`/api/v1/following/new-releases/recent?days=${days}&limit=${limit}&include_owned=${includeOwned}`,
		newReleasesUnseenCount: () => '/api/v1/following/new-releases/unseen-count',
		markNewReleasesSeen: () => '/api/v1/following/new-releases/seen',
		events: () => '/api/v1/following/events',
		concerts: () => '/api/v1/following/concerts',
		concertCities: () => '/api/v1/following/concerts/cities',
		concertCitySearch: (q: string) =>
			`/api/v1/following/concerts/city-search?q=${encodeURIComponent(q)}`,
		concertsUnseenCount: () => '/api/v1/following/concerts/unseen-count',
		markConcertsSeen: () => '/api/v1/following/concerts/seen'
	},
	album: {
		basic: (id: string) => `/api/v1/albums/${id}`,
		tracks: (id: string) => `/api/v1/albums/${id}/tracks`,
		purchaseOptions: (id: string) => `/api/v1/albums/${id}/purchase-options`
	},
	library: {
		mbids: () => '/api/v1/library/mbids',
		albums: (page = 1, sort = 'recent', q?: string, format?: string, pageSize = 50) => {
			let url = `/api/v1/library/albums?page=${page}&page_size=${pageSize}&sort=${sort}`;
			if (q) url += `&q=${encodeURIComponent(q)}`;
			if (format) url += `&format=${encodeURIComponent(format)}`;
			return url;
		},
		tracks: (limit = 48, offset = 0, sort = 'recent', q?: string) => {
			let url = `/api/v1/library/tracks?limit=${limit}&offset=${offset}&sort=${sort}`;
			if (q) url += `&q=${encodeURIComponent(q)}`;
			return url;
		},
		artists: (limit = 50, offset = 0, sortBy = 'name', sortOrder = 'asc', q?: string) => {
			let url = `/api/v1/library/artists?limit=${limit}&offset=${offset}&sort_by=${sortBy}&sort_order=${sortOrder}`;
			if (q) url += `&q=${encodeURIComponent(q)}`;
			return url;
		},
		album: (mbid: string) => `/api/v1/library/albums/${mbid}/status`,
		albumTracks: (mbid: string) => `/api/v1/library/albums/${mbid}/tracks`,
		stats: () => '/api/v1/library/stats',
		scanSchedule: () => '/api/v1/settings/library/schedule',
		rescanAlbum: (mbid: string) => `/api/v1/library/albums/${mbid}/rescan`,
		reidentifyAlbum: (mbid: string) => `/api/v1/library/albums/${mbid}/reidentify`,
		updateTrackTags: (fileId: string) => `/api/v1/library/tracks/${fileId}`,
		trackTags: (fileId: string) => `/api/v1/library/tracks/${fileId}/tags`,
		removeTrack: (fileId: string) => `/api/v1/library/tracks/${fileId}`,
		scanStart: () => '/api/v1/library/scan/start',
		scanCancel: () => '/api/v1/library/scan/cancel',
		scanStatus: () => '/api/v1/library/scan/status',
		scanStream: () => '/api/v1/library/scan/stream',
		unmatched: () => '/api/v1/library/scan/unmatched',
		resolveUnmatched: (id: number) => `/api/v1/library/scan/unmatched/${id}/resolve`,
		resolveUnmatchedBatch: () => '/api/v1/library/scan/unmatched/resolve-batch',
		settings: () => '/api/v1/settings/library',
		addPath: () => '/api/v1/settings/library/paths',
		removePath: (path: string) => `/api/v1/settings/library/paths?path=${encodeURIComponent(path)}`,
		removeAlbumPreview: (mbid: string) => `/api/v1/library/album/${mbid}/removal-preview`,
		removeAlbum: (mbid: string) => `/api/v1/library/album/${mbid}`,
		resolveTracks: () => '/api/v1/library/resolve-tracks'
	},
	search: {
		artists: (query: string) => `/api/v1/search/artists?q=${encodeURIComponent(query)}`,
		albums: (query: string) => `/api/v1/search/albums?q=${encodeURIComponent(query)}`,
		suggest: (query: string, limit = 5) =>
			`/api/v1/search/suggest?q=${encodeURIComponent(query.trim())}&limit=${limit}`
	},
	system: {
		health: () => '/api/v1/system/health'
	},
	home: () => '/api/v1/home',
	homeIntegrationStatus: () => '/api/v1/home/integration-status',
	discover: () => '/api/v1/discover',
	discoverRefresh: () => '/api/v1/discover/refresh',
	discoverQueue: () => '/api/v1/discover/queue',
	discoverQueueStatus: () => '/api/v1/discover/queue/status',
	discoverQueueGenerate: () => '/api/v1/discover/queue/generate',
	discoverQueueEnrich: (mbid: string) => `/api/v1/discover/queue/enrich/${mbid}`,
	discoverQueueIgnore: () => '/api/v1/discover/queue/ignore',
	discoverQueueIgnored: () => '/api/v1/discover/queue/ignored',
	discoverQueueValidate: () => '/api/v1/discover/queue/validate',
	discoverQueueYoutubeSearch: (artist: string, album: string) =>
		`/api/v1/discover/queue/youtube-search?artist=${encodeURIComponent(artist)}&album=${encodeURIComponent(album)}`,
	discoverQueueYoutubeTrackSearch: (artist: string, track: string) =>
		`/api/v1/discover/queue/youtube-track-search?artist=${encodeURIComponent(artist)}&track=${encodeURIComponent(track)}`,
	discoverQueueYoutubeQuota: () => '/api/v1/discover/queue/youtube-quota',
	discoverQueueYoutubeCacheCheck: () => '/api/v1/discover/queue/youtube-cache-check',
	discoverRadio: () => '/api/v1/discover/radio',
	discoverRadioPlan: () => '/api/v1/discover/radio/plan',
	discoverBatches: () => '/api/v1/discover/batches',
	discoverBatch: (id: string) => `/api/v1/discover/batches/${id}`,
	discoverBatchRemove: (id: string, removeAlbums: boolean) =>
		`/api/v1/discover/batches/${id}?remove_albums=${removeAlbums}`,
	discoverTrackPreview: (artist: string, track: string) =>
		`/api/v1/discover/track-preview?artist=${encodeURIComponent(artist)}&track=${encodeURIComponent(track)}`,
	discoverAlbumPreview: (artist: string, album: string) =>
		`/api/v1/discover/album-preview?artist=${encodeURIComponent(artist)}&album=${encodeURIComponent(album)}`,
	discoverPlaylistSuggestions: () => '/api/v1/discover/playlist-suggestions',
	discoverGenreDetail: (tag: string) => `/api/v1/discover/genres/${encodeURIComponent(tag)}`,
	discoverTasteGraph: () => '/api/v1/discover/taste-graph',
	youtube: {
		generate: () => '/api/v1/youtube/generate',
		link: (albumId: string) => `/api/v1/youtube/link/${albumId}`,
		links: () => '/api/v1/youtube/links',
		deleteLink: (albumId: string) => `/api/v1/youtube/link/${albumId}`,
		updateLink: (albumId: string) => `/api/v1/youtube/link/${albumId}`,
		manual: () => '/api/v1/youtube/manual',
		generateTrack: () => '/api/v1/youtube/generate-track',
		generateTracks: () => '/api/v1/youtube/generate-tracks',
		trackLinks: (albumId: string) => `/api/v1/youtube/track-links/${albumId}`,
		deleteTrackLink: (albumId: string, discNumber: number, trackNumber: number) =>
			`/api/v1/youtube/track-link/${albumId}/${discNumber}/${trackNumber}`,
		quota: () => '/api/v1/youtube/quota'
	},
	settings: () => '/api/v1/settings',
	settingsPrimarySource: () => '/api/v1/settings/primary-source',
	settingsNavidrome: () => '/api/v1/settings/navidrome',
	settingsNavidromeVerify: () => '/api/v1/settings/navidrome/verify',
	settingsPlex: () => '/api/v1/settings/plex',
	settingsPlexVerify: () => '/api/v1/settings/plex/verify',
	settingsPlexLibraries: () => '/api/v1/settings/plex/libraries',
	plexAuthPin: () => '/api/v1/plex/auth/pin',
	plexAuthPoll: (pinId: number) => `/api/v1/plex/auth/poll?pin_id=${pinId}`,
	settingsLocalFiles: () => '/api/v1/settings/local-files',
	settingsLocalFilesVerify: () => '/api/v1/settings/local-files/verify',
	settingsMusicbrainz: () => '/api/v1/settings/musicbrainz',
	settingsMusicbrainzVerify: () => '/api/v1/settings/musicbrainz/verify',
	settingsGetIt: () => '/api/v1/settings/get-it',
	settingsFreeMusic: () => '/api/v1/settings/free-music',
	profile: {
		get: () => '/api/v1/profile',
		update: () => '/api/v1/profile',
		avatarUpload: () => '/api/v1/profile/avatar',
		avatar: (userId: string) => `/api/v1/profile/avatar/${userId}`,
		updateUsername: () => '/api/v1/profile/username',
		updateEmail: () => '/api/v1/profile/email',
		changePassword: () => '/api/v1/profile/password',
		setPassword: () => '/api/v1/profile/set-password'
	},
	me: {
		connections: () => '/api/v1/me/connections',
		connection: (service: string) => `/api/v1/me/connections/${service}`,
		scrobblePreferences: () => '/api/v1/me/scrobble-preferences',
		sectionPrefs: () => '/api/v1/me/section-prefs',
		lastfmAuthToken: () => '/api/v1/me/connections/lastfm/auth/token',
		lastfmAuthSession: () => '/api/v1/me/connections/lastfm/auth/session',
		listenbrainz: () => '/api/v1/me/connections/listenbrainz',
		spotifyAuthUrl: () => '/api/v1/me/connections/spotify/auth/url',
		spotifyPlaylists: () => '/api/v1/me/spotify/playlists',
		spotifyImport: (playlistId: string) => `/api/v1/me/spotify/playlists/${playlistId}/import`,
		personalMixRefresh: () => '/api/v1/me/personal-mix/refresh'
	},
	scrobble: {
		nowPlaying: () => '/api/v1/scrobble/now-playing',
		submit: () => '/api/v1/scrobble/submit'
	},
	nowPlaying: {
		report: () => '/api/v1/now-playing',
		events: () => '/api/v1/now-playing/events'
	},
	playlists: {
		list: () => '/api/v1/playlists',
		create: () => '/api/v1/playlists',
		generate: () => '/api/v1/playlists/generate',
		detail: (id: string) => `/api/v1/playlists/${id}`,
		update: (id: string) => `/api/v1/playlists/${id}`,
		delete: (id: string) => `/api/v1/playlists/${id}`,
		addTracks: (id: string) => `/api/v1/playlists/${id}/tracks`,
		removeTracks: (id: string) => `/api/v1/playlists/${id}/tracks/remove`,
		removeTrack: (id: string, trackId: string) => `/api/v1/playlists/${id}/tracks/${trackId}`,
		updateTrack: (id: string, trackId: string) => `/api/v1/playlists/${id}/tracks/${trackId}`,
		reorderTrack: (id: string) => `/api/v1/playlists/${id}/tracks/reorder`,
		uploadCover: (id: string) => `/api/v1/playlists/${id}/cover`,
		getCover: (id: string) => `/api/v1/playlists/${id}/cover`,
		deleteCover: (id: string) => `/api/v1/playlists/${id}/cover`,
		checkTracks: () => '/api/v1/playlists/check-tracks',
		resolveSources: (id: string) => `/api/v1/playlists/${id}/resolve-sources`,
		share: (id: string) => `/api/v1/playlists/${id}/share`,
		requestMissing: (id: string) => `/api/v1/playlists/${id}/request-missing`
	},
	stream: {
		jellyfin: (itemId: string) => `/api/v1/stream/jellyfin/${itemId}`,
		jellyfinStart: (itemId: string) => `/api/v1/stream/jellyfin/${itemId}/start`,
		jellyfinProgress: (itemId: string) => `/api/v1/stream/jellyfin/${itemId}/progress`,
		jellyfinStop: (itemId: string) => `/api/v1/stream/jellyfin/${itemId}/stop`,
		navidrome: (id: string) => `/api/v1/stream/navidrome/${id}`,
		navidromeScrobble: (id: string) => `/api/v1/stream/navidrome/${id}/scrobble`,
		navidromeNowPlaying: (id: string) => `/api/v1/stream/navidrome/${id}/now-playing`,
		navidromeStopped: (id: string) => `/api/v1/stream/navidrome/${id}/stopped`,
		plex: (partKey: string) => `/api/v1/stream/plex/${partKey}`,
		plexScrobble: (ratingKey: string) => `/api/v1/stream/plex/${ratingKey}/scrobble`,
		plexNowPlaying: (ratingKey: string) => `/api/v1/stream/plex/${ratingKey}/now-playing`,
		plexStopped: (ratingKey: string) => `/api/v1/stream/plex/${ratingKey}/stopped`,
		local: (trackId: number | string) => `/api/v1/stream/local/${trackId}`
	},
	download: {
		localTrack: (trackId: string) => `/api/v1/download/local/track/${trackId}`,
		localAlbum: (albumId: number) => `/api/v1/download/local/album/${albumId}`,
		localAlbumByMbid: (mbid: string) => `/api/v1/download/local/album/mbid/${mbid}`
	},
	downloadClient: {
		config: () => '/api/v1/download-client/config',
		test: () => '/api/v1/download-client/test',
		status: () => '/api/v1/download-client/status'
	},
	indexers: {
		list: () => '/api/v1/indexers',
		create: () => '/api/v1/indexers',
		update: (id: string) => `/api/v1/indexers/${id}`,
		remove: (id: string) => `/api/v1/indexers/${id}`,
		reorder: () => '/api/v1/indexers/reorder',
		test: () => '/api/v1/indexers/test'
	},
	lidarrImport: {
		config: () => '/api/v1/lidarr-import/config',
		test: () => '/api/v1/lidarr-import/test',
		status: () => '/api/v1/lidarr-import/status',
		artists: () => '/api/v1/lidarr-import/artists',
		import: () => '/api/v1/lidarr-import/import'
	},
	freeMusic: {
		tasks: (all: boolean = false) => `/api/v1/free-music/tasks${all ? '?all=true' : ''}`,
		task: (id: string) => `/api/v1/free-music/tasks/${id}`,
		cancel: (id: string) => `/api/v1/free-music/tasks/${id}/cancel`,
		retry: (id: string) => `/api/v1/free-music/tasks/${id}/retry`
	},
	plugins: {
		list: () => '/api/v1/plugins',
		install: () => '/api/v1/plugins/install',
		update: (name: string) => `/api/v1/plugins/${name}`,
		uninstall: (name: string) => `/api/v1/plugins/${name}`
	},
	dropImport: {
		uploads: () => '/api/v1/import/uploads',
		jobs: (all: boolean = false) => `/api/v1/import/jobs${all ? '?all=true' : ''}`,
		job: (jobId: string) => `/api/v1/import/jobs/${jobId}`,
		match: (itemId: number) => `/api/v1/import/items/${itemId}/match`,
		discard: (itemId: number) => `/api/v1/import/items/${itemId}/discard`
	},
	downloadClients: {
		sabnzbd: () => '/api/v1/download-clients/sabnzbd',
		sabnzbdTest: () => '/api/v1/download-clients/sabnzbd/test',
		policy: () => '/api/v1/download-clients/policy',
		sourcePriority: () => '/api/v1/download-clients/source-priority',
		wanted: () => '/api/v1/download-clients/wanted'
	},
	connectApps: {
		settings: () => '/api/v1/connect-apps/settings',
		appPasswords: () => '/api/v1/connect-apps/app-passwords',
		appPassword: (id: string) => `/api/v1/connect-apps/app-passwords/${id}`,
		adminAppPasswords: () => '/api/v1/connect-apps/admin/app-passwords',
		adminAppPassword: (id: string) => `/api/v1/connect-apps/admin/app-passwords/${id}`
	},
	downloads: {
		searchAlbum: () => '/api/v1/downloads/search/album',
		searchJob: (jobId: string) => `/api/v1/downloads/search/${jobId}`,
		pick: (jobId: string) => `/api/v1/downloads/search/${jobId}/pick`,
		dismissReview: (jobId: string) => `/api/v1/downloads/search/${jobId}/dismiss`,
		cancelSearch: (jobId: string) => `/api/v1/downloads/search/${jobId}/cancel`,
		searchStream: (jobId: string) => `/api/v1/downloads/search/stream?job_id=${jobId}`,
		quarantine: () => '/api/v1/downloads/quarantine',
		quarantineDelete: (id: number) => `/api/v1/downloads/quarantine/${id}`,
		list: (status?: string, page = 1, pageSize = 100, releaseGroupMbid?: string) => {
			const params = new URLSearchParams();
			if (status) params.set('status', status);
			if (releaseGroupMbid) params.set('release_group_mbid', releaseGroupMbid);
			params.set('page', String(page));
			params.set('page_size', String(pageSize));
			return `/api/v1/downloads?${params.toString()}`;
		},
		get: (taskId: string) => `/api/v1/downloads/${taskId}`,
		stream: (taskId: string) => `/api/v1/downloads/${taskId}/stream`,
		cancel: (taskId: string) => `/api/v1/downloads/${taskId}/cancel`,
		retry: (taskId: string) => `/api/v1/downloads/${taskId}/retry`,
		clear: () => '/api/v1/downloads/clear',
		stopAllRetries: () => '/api/v1/downloads/stop-all-retries',
		retryAllFailed: () => '/api/v1/downloads/retry-all-failed',
		held: (releaseGroupMbid?: string) => {
			const params = new URLSearchParams();
			if (releaseGroupMbid) params.set('release_group_mbid', releaseGroupMbid);
			const qs = params.toString();
			return `/api/v1/downloads/held${qs ? `?${qs}` : ''}`;
		},
		heldImport: (id: number) => `/api/v1/downloads/held/${id}/import`,
		heldDiscard: (id: number) => `/api/v1/downloads/held/${id}/discard`,
		heldAudio: (id: number) => `/api/v1/downloads/held/${id}/audio`,
		reimport: (taskId: string) => `/api/v1/downloads/${taskId}/reimport`,
		cutoffUnmet: () => '/api/v1/downloads/cutoff-unmet',
		upgradeAlbum: () => '/api/v1/downloads/upgrade/album',
		upgradeTrack: () => '/api/v1/downloads/upgrade/track'
	},
	requests: {
		new: () => '/api/v1/requests/new',
		autoDownloadApprovals: () => '/api/v1/requests/auto-download-approvals',
		approveAutoDownload: (userId: string, mbid: string) =>
			`/api/v1/requests/auto-download-approvals/${userId}/${mbid}/approve`,
		rejectAutoDownload: (userId: string, mbid: string) =>
			`/api/v1/requests/auto-download-approvals/${userId}/${mbid}/reject`,
		revokeAutoDownload: (userId: string, mbid: string) =>
			`/api/v1/requests/auto-download-approvals/${userId}/${mbid}/revoke`,
		autoDownloadApprovalBatches: () => '/api/v1/requests/auto-download-approval-batches',
		approveAutoDownloadBatch: (batchId: string) =>
			`/api/v1/requests/auto-download-approval-batches/${batchId}/approve`,
		rejectAutoDownloadBatch: (batchId: string) =>
			`/api/v1/requests/auto-download-approval-batches/${batchId}/reject`,
		personalMixApprovals: () => '/api/v1/requests/personal-mix-approvals',
		approvePersonalMix: (userId: string) =>
			`/api/v1/requests/personal-mix-approvals/${userId}/approve`,
		rejectPersonalMix: (userId: string) =>
			`/api/v1/requests/personal-mix-approvals/${userId}/reject`,
		revokePersonalMix: (userId: string) =>
			`/api/v1/requests/personal-mix-approvals/${userId}/revoke`,
		wanted: () => '/api/v1/requests/wanted',
		wantedStop: (mbid: string) => `/api/v1/requests/wanted/${mbid}/stop`,
		wantedResume: (mbid: string) => `/api/v1/requests/wanted/${mbid}/resume`,
		wantedSeen: (mbid: string) => `/api/v1/requests/wanted/${mbid}/seen`
	},
	tracks: {
		request: (recordingMbid: string) => `/api/v1/tracks/${recordingMbid}/request`
	},
	jellyfinLibrary: {
		albumMatch: (mbid: string) => `/api/v1/jellyfin/albums/match/${mbid}`,
		albums: (
			limit = 50,
			offset = 0,
			sortBy = 'SortName',
			genre?: string,
			sortOrder = 'Ascending',
			year?: number,
			tags?: string,
			studios?: string
		) => {
			let url = `/api/v1/jellyfin/albums?limit=${limit}&offset=${offset}&sort_by=${sortBy}&sort_order=${sortOrder}`;
			if (genre) url += `&genre=${encodeURIComponent(genre)}`;
			if (year) url += `&year=${year}`;
			if (tags) url += `&tags=${encodeURIComponent(tags)}`;
			if (studios) url += `&studios=${encodeURIComponent(studios)}`;
			return url;
		},
		albumDetail: (id: string) => `/api/v1/jellyfin/albums/${id}`,
		albumTracks: (id: string) => `/api/v1/jellyfin/albums/${id}/tracks`,
		search: (query: string) => `/api/v1/jellyfin/search?q=${encodeURIComponent(query)}`,
		artists: (limit = 50, offset = 0) => `/api/v1/jellyfin/artists?limit=${limit}&offset=${offset}`,
		recent: () => '/api/v1/jellyfin/recent',
		favorites: () => '/api/v1/jellyfin/favorites',
		genres: () => '/api/v1/jellyfin/genres',
		stats: () => '/api/v1/jellyfin/stats',
		hub: () => '/api/v1/jellyfin/hub',
		recentlyAdded: (limit = 20) => `/api/v1/jellyfin/recently-added?limit=${limit}`,
		mostPlayedArtists: (limit = 10) => `/api/v1/jellyfin/most-played/artists?limit=${limit}`,
		mostPlayedAlbums: (limit = 10) => `/api/v1/jellyfin/most-played/albums?limit=${limit}`,
		playlists: (limit = 50) => `/api/v1/jellyfin/playlists?limit=${limit}`,
		playlistDetail: (id: string) => `/api/v1/jellyfin/playlists/${id}`,
		playlistImport: (id: string) => `/api/v1/jellyfin/playlists/${id}/import`,
		instantMix: (itemId: string, limit = 50) =>
			`/api/v1/jellyfin/instant-mix/${itemId}?limit=${limit}`,
		instantMixByArtist: (artistId: string, limit = 50) =>
			`/api/v1/jellyfin/instant-mix/artist/${artistId}?limit=${limit}`,
		instantMixByGenre: (genre: string, limit = 50) =>
			`/api/v1/jellyfin/instant-mix/genre?genre=${encodeURIComponent(genre)}&limit=${limit}`,
		sessions: () => '/api/v1/jellyfin/sessions',
		similar: (itemId: string, limit = 10) => `/api/v1/jellyfin/similar/${itemId}?limit=${limit}`,
		lyrics: (itemId: string) => `/api/v1/jellyfin/lyrics/${itemId}`,
		favoritesExpanded: (limit = 50) => `/api/v1/jellyfin/favorites/expanded?limit=${limit}`,
		filters: () => '/api/v1/jellyfin/filters',
		artistsBrowse: (
			limit = 48,
			offset = 0,
			sortBy = 'SortName',
			sortOrder = 'Ascending',
			search = ''
		) => {
			let url = `/api/v1/jellyfin/artists/browse?limit=${limit}&offset=${offset}&sort_by=${sortBy}&sort_order=${sortOrder}`;
			if (search) url += `&search=${encodeURIComponent(search)}`;
			return url;
		},
		tracks: (limit = 48, offset = 0, sortBy = 'SortName', sortOrder = 'Ascending', search = '') => {
			let url = `/api/v1/jellyfin/tracks?limit=${limit}&offset=${offset}&sort_by=${sortBy}&sort_order=${sortOrder}`;
			if (search) url += `&search=${encodeURIComponent(search)}`;
			return url;
		},
		artistsIndex: () => '/api/v1/jellyfin/artists/index',
		genreSongs: (genres: string | string[], limit = 50, offset = 0) => {
			const g = Array.isArray(genres) ? genres.join('|') : genres;
			return `/api/v1/jellyfin/genres/songs?genre=${encodeURIComponent(g)}&limit=${limit}&offset=${offset}`;
		}
	},
	navidromeLibrary: {
		albums: () => '/api/v1/navidrome/albums',
		albumDetail: (id: string) => `/api/v1/navidrome/albums/${id}`,
		artists: () => '/api/v1/navidrome/artists',
		artistDetail: (id: string) => `/api/v1/navidrome/artists/${id}`,
		search: (q: string) => `/api/v1/navidrome/search?q=${encodeURIComponent(q)}`,
		recent: () => '/api/v1/navidrome/recent',
		favorites: () => '/api/v1/navidrome/favorites',
		genres: () => '/api/v1/navidrome/genres',
		stats: () => '/api/v1/navidrome/stats',
		albumMatch: (albumId: string) => `/api/v1/navidrome/album-match/${albumId}`,
		hub: () => '/api/v1/navidrome/hub',
		favoritesExpanded: () => '/api/v1/navidrome/favorites/expanded',
		playlists: (limit = 50) => `/api/v1/navidrome/playlists?limit=${limit}`,
		playlistDetail: (id: string) => `/api/v1/navidrome/playlists/${id}`,
		playlistImport: (id: string) => `/api/v1/navidrome/playlists/${id}/import`,
		random: (size = 20, genre?: string) => {
			let url = `/api/v1/navidrome/random?size=${size}`;
			if (genre) url += `&genre=${encodeURIComponent(genre)}`;
			return url;
		},
		nowPlaying: () => '/api/v1/navidrome/now-playing',
		topSongs: (artistName: string, count = 20) =>
			`/api/v1/navidrome/top-songs/${encodeURIComponent(artistName)}?count=${count}`,
		similarSongs: (songId: string, count = 20) =>
			`/api/v1/navidrome/similar-songs/${songId}?count=${count}`,
		artistInfo: (artistId: string) => `/api/v1/navidrome/artist-info/${artistId}`,
		albumInfo: (albumId: string) => `/api/v1/navidrome/album-info/${albumId}`,
		lyrics: (songId: string, artist = '', title = '') => {
			let url = `/api/v1/navidrome/lyrics/${songId}`;
			const params: string[] = [];
			if (artist) params.push(`artist=${encodeURIComponent(artist)}`);
			if (title) params.push(`title=${encodeURIComponent(title)}`);
			if (params.length) url += `?${params.join('&')}`;
			return url;
		},
		artistsIndex: () => '/api/v1/navidrome/artists/index',
		genreSongs: (genre: string, count = 50, offset = 0) =>
			`/api/v1/navidrome/genres/${encodeURIComponent(genre)}/songs?count=${count}&offset=${offset}`,
		multiGenreSongs: (genres: string[], count = 50, offset = 0) =>
			`/api/v1/navidrome/genres/songs?genres=${encodeURIComponent(genres.join(','))}&count=${count}&offset=${offset}`,
		musicFolders: () => '/api/v1/navidrome/music-folders',
		artistsBrowse: (limit = 48, offset = 0, search = '') => {
			let url = `/api/v1/navidrome/artists/browse?limit=${limit}&offset=${offset}`;
			if (search) url += `&search=${encodeURIComponent(search)}`;
			return url;
		},
		tracks: (limit = 48, offset = 0, search = '') => {
			let url = `/api/v1/navidrome/tracks?limit=${limit}&offset=${offset}`;
			if (search) url += `&search=${encodeURIComponent(search)}`;
			return url;
		}
	},
	plexLibrary: {
		albums: (
			limit = 48,
			offset = 0,
			sortBy = 'name',
			genre?: string,
			sortOrder?: string,
			mood?: string,
			decade?: string
		) => {
			let url = `/api/v1/plex/albums?limit=${limit}&offset=${offset}&sort_by=${sortBy}`;
			if (sortOrder) url += `&sort_order=${sortOrder}`;
			if (genre) url += `&genre=${encodeURIComponent(genre)}`;
			if (mood) url += `&mood=${encodeURIComponent(mood)}`;
			if (decade) url += `&decade=${encodeURIComponent(decade)}`;
			return url;
		},
		albumDetail: (id: string) => `/api/v1/plex/albums/${id}`,
		search: (q: string) => `/api/v1/plex/search?q=${encodeURIComponent(q)}`,
		recent: (limit = 20) => `/api/v1/plex/recent?limit=${limit}`,
		genres: () => '/api/v1/plex/genres',
		moods: () => '/api/v1/plex/moods',
		stats: () => '/api/v1/plex/stats',
		thumb: (ratingKey: string, size = 500) => `/api/v1/plex/thumb/${ratingKey}?size=${size}`,
		albumMatch: (albumId: string) => `/api/v1/plex/album-match/${albumId}`,
		hub: () => '/api/v1/plex/hub',
		recentlyAdded: (limit = 20) => `/api/v1/plex/recently-added?limit=${limit}`,
		playlists: (limit = 50) => `/api/v1/plex/playlists?limit=${limit}`,
		playlistDetail: (id: string) => `/api/v1/plex/playlists/${id}`,
		playlistImport: (id: string) => `/api/v1/plex/playlists/${id}/import`,
		discovery: (count = 10) => `/api/v1/plex/discovery?count=${count}`,
		sessions: () => '/api/v1/plex/sessions',
		history: (limit = 50, offset = 0) => `/api/v1/plex/history?limit=${limit}&offset=${offset}`,
		analytics: () => '/api/v1/plex/analytics',
		artistsBrowse: (limit = 48, offset = 0, sort = 'titleSort:asc', search = '') => {
			let url = `/api/v1/plex/artists/browse?limit=${limit}&offset=${offset}&sort=${encodeURIComponent(sort)}`;
			if (search) url += `&search=${encodeURIComponent(search)}`;
			return url;
		},
		tracks: (limit = 48, offset = 0, sort = 'titleSort:asc', search = '') => {
			let url = `/api/v1/plex/tracks?limit=${limit}&offset=${offset}&sort=${encodeURIComponent(sort)}`;
			if (search) url += `&search=${encodeURIComponent(search)}`;
			return url;
		},
		artistsIndex: () => '/api/v1/plex/artists/index',
		genreSongs: (genre: string, limit = 50, offset = 0) =>
			`/api/v1/plex/genres/songs?genre=${encodeURIComponent(genre)}&limit=${limit}&offset=${offset}`
	},
	version: {
		info: () => '/api/v1/version',
		checkUpdate: () => '/api/v1/version/check-update',
		releases: () => '/api/v1/version/releases'
	},
	local: {
		albumMatch: (mbid: string) => `/api/v1/local/albums/match/${mbid}`,
		albums: (
			limit = 50,
			offset = 0,
			sortBy = 'name',
			q?: string,
			sortOrder = 'asc',
			decade?: number
		) => {
			let url = `/api/v1/local/albums?limit=${limit}&offset=${offset}&sort_by=${sortBy}&sort_order=${sortOrder}`;
			if (q) url += `&q=${encodeURIComponent(q)}`;
			if (decade != null) url += `&decade=${decade}`;
			return url;
		},
		albumTracks: (mbid: string) => `/api/v1/local/albums/${mbid}/tracks`,
		search: (query: string) => `/api/v1/local/search?q=${encodeURIComponent(query)}`,
		recent: () => '/api/v1/local/recent',
		stats: () => '/api/v1/local/stats',
		suggestions: (limit = 12, decade?: number) => {
			let url = `/api/v1/local/suggestions?limit=${limit}`;
			if (decade != null) url += `&decade=${decade}`;
			return url;
		},
		decades: () => '/api/v1/local/decades'
	}
} as const;
