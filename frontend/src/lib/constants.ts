/** Album vs track request lane, carried on request query strings. */
export type RequestKind = 'album' | 'track';

export const AUTH_FREE_PATHS = ['/login', '/setup', '/auth/callback', '/recover-password'];

// concert distances are stored in km but displayed in miles (owner decision U6)
export const KM_PER_MILE = 1.609;

const CACHE_KEY_GROUPS = {
	core: {
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
		NAVIDROME_FOLDER_SCOPE: 'droppedneedle_navidrome_folder_scope',
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
// URL builders for calls the v3 server has no route for yet. This file is
// the waiting-on-backend exception to the transport lint rule
// (eslint.config.js): a builder leaves when its route lands and the call
// moves onto a typed v3 registry template.
export const API = {
	library: {
		membership: () => '/api/v1/library/membership',
		album: (mbid: string) => `/api/v1/library/albums/${mbid}/status`,
		cachedAlbumArtwork: (albumId: string, coverVersion: number) =>
			`/api/v1/library/albums/${encodeURIComponent(albumId)}/artwork/cached?v=${coverVersion}`,
		rescanAlbum: (mbid: string) => `/api/v1/library/albums/${mbid}/rescan`,
		reenableAlbumManagement: (albumId: string) =>
			`/api/v1/library/albums/${encodeURIComponent(albumId)}/management/re-enable`,
		editionConversionPreflight: (albumId: string) =>
			`/api/v1/library/albums/${encodeURIComponent(albumId)}/edition-conversions/preflight`,
		editionConversion: (jobId: string) =>
			`/api/v1/library/edition-conversions/${encodeURIComponent(jobId)}`,
		editionConversionPreview: (jobId: string) =>
			`/api/v1/library/edition-conversions/${encodeURIComponent(jobId)}/preview`,
		editionConversionStart: (jobId: string) =>
			`/api/v1/library/edition-conversions/${encodeURIComponent(jobId)}/start`,
		editionConversionRetry: (jobId: string) =>
			`/api/v1/library/edition-conversions/${encodeURIComponent(jobId)}/retry`,
		editionConversionRecheck: (jobId: string) =>
			`/api/v1/library/edition-conversions/${encodeURIComponent(jobId)}/recheck`,
		editionConversionCancel: (jobId: string) =>
			`/api/v1/library/edition-conversions/${encodeURIComponent(jobId)}/cancel`,
		removeTrack: (fileId: string) => `/api/v1/library/tracks/${fileId}`,
		reviews: (
			params: {
				cursor?: string;
				limit?: number;
				state?: string;
				reasonCode?: string;
				rootId?: string;
				policy?: string;
				search?: string;
				sort?: string;
				candidateAvailable?: boolean;
				exclude_active_jobs?: boolean;
			} = {}
		) => {
			const query = new URLSearchParams();
			if (params.cursor) query.set('cursor', params.cursor);
			if (params.limit !== undefined) query.set('limit', String(params.limit));
			if (params.state) query.set('state', params.state);
			if (params.reasonCode) query.set('reason_code', params.reasonCode);
			if (params.rootId) query.set('root_id', params.rootId);
			if (params.policy) query.set('policy', params.policy);
			if (params.search) query.set('search', params.search);
			if (params.sort) query.set('sort', params.sort);
			if (params.candidateAvailable) query.set('candidate_available', 'true');
			if (params.exclude_active_jobs) query.set('exclude_active_jobs', 'true');
			return `/api/v1/library/reviews${query.size ? `?${query.toString()}` : ''}`;
		},
		review: (reviewId: string) => `/api/v1/library/reviews/${reviewId}`,
		reviewDetachKeepTagged: (reviewId: string) =>
			`/api/v1/library/reviews/${reviewId}/detach-and-keep-tagged`,
		reviewExclude: (reviewId: string) => `/api/v1/library/reviews/${reviewId}/exclude`,
		reviewRestore: (reviewId: string) => `/api/v1/library/reviews/${reviewId}/restore`,
		reviewDismiss: (reviewId: string) => `/api/v1/library/reviews/${reviewId}/dismiss`,
		reviewRetry: (reviewId: string) => `/api/v1/library/reviews/${reviewId}/retry`,
		bulkReviewPreview: () => '/api/v1/library/reviews/bulk-preview',
		bulkReviewApply: () => '/api/v1/library/reviews/bulk-apply',
		previewAlbumSplit: (albumId: string) => `/api/v1/library/albums/${albumId}/split-preview`,
		splitAlbum: (albumId: string) => `/api/v1/library/albums/${albumId}/split`,
		previewAlbumMerge: () => '/api/v1/library/albums/merge-preview',
		mergeAlbums: () => '/api/v1/library/albums/merge',
		previewTrackMove: () => '/api/v1/library/tracks/move-preview',
		moveTracks: () => '/api/v1/library/tracks/move',
		previewResetAlbumGrouping: (albumId: string) =>
			`/api/v1/library/albums/${albumId}/reset-grouping-preview`,
		resetAlbumGrouping: (albumId: string) => `/api/v1/library/albums/${albumId}/reset-grouping`,
		previewArtistMerge: () => '/api/v1/library/artists/merge-preview',
		mergeArtists: () => '/api/v1/library/artists/merge',
		artistReconciliation: () => '/api/v1/library/artists/reconciliation',
		artistDuplicateGroups: (
			params: {
				limit?: number;
				cursor?: string;
				state?: string;
				search?: string;
			} = {}
		) => {
			const query = new URLSearchParams();
			if (params.limit !== undefined) query.set('limit', String(params.limit));
			if (params.cursor) query.set('cursor', params.cursor);
			if (params.state) query.set('state', params.state);
			if (params.search) query.set('search', params.search);
			const suffix = query.size ? `?${query.toString()}` : '';
			return `/api/v1/library/artists/duplicate-groups${suffix}`;
		},
		artistDuplicateGroup: (groupId: string) =>
			`/api/v1/library/artists/duplicate-groups/${groupId}`,
		dismissArtistDuplicateGroup: (groupId: string) =>
			`/api/v1/library/artists/duplicate-groups/${groupId}/dismiss`,
		identityRepairs: (limit?: number, cursor?: string) => {
			const query = new URLSearchParams();
			if (limit !== undefined) query.set('limit', String(limit));
			if (cursor) query.set('cursor', cursor);
			let url = '/api/v1/library/identity-repairs';
			if (query.size) url += `?${query.toString()}`;
			return url;
		},
		identityRepairEstimate: (rootIds: string[]) => {
			const query = new URLSearchParams();
			for (const rootId of rootIds) query.append('root_id', rootId);
			const suffix = query.size ? `?${query.toString()}` : '';
			return `/api/v1/library/identity-repairs/estimate${suffix}`;
		},
		identityRepair: (jobId: string) => `/api/v1/library/identity-repairs/${jobId}`,
		identityRepairFindings: (
			jobId: string,
			limit?: number,
			cursor?: string,
			findingCategory?: string
		) => {
			const query = new URLSearchParams();
			if (limit !== undefined) query.set('limit', String(limit));
			if (cursor) query.set('cursor', cursor);
			if (findingCategory) query.set('finding_category', findingCategory);
			let url = `/api/v1/library/identity-repairs/${jobId}/findings`;
			if (query.size) url += `?${query.toString()}`;
			return url;
		},
		applyIdentityRepair: (jobId: string) => `/api/v1/library/identity-repairs/${jobId}/apply`,
		identityPreparations: (limit?: number, cursor?: string) => {
			const query = new URLSearchParams();
			if (limit !== undefined) query.set('limit', String(limit));
			if (cursor) query.set('cursor', cursor);
			const path = '/api/v1/library/management/identity-preparations';
			return query.size ? `${path}?${query.toString()}` : path;
		},
		identityPreparationEstimate: (rootIds: string[]) => {
			const query = new URLSearchParams();
			for (const rootId of rootIds) query.append('root_id', rootId);
			const path = '/api/v1/library/management/identity-preparations/estimate';
			return query.size ? `${path}?${query.toString()}` : path;
		},
		identityPreparationFindings: (
			jobId: string,
			limit?: number,
			cursor?: string,
			findingCategory?: string
		) => {
			const query = new URLSearchParams();
			if (limit !== undefined) query.set('limit', String(limit));
			if (cursor) query.set('cursor', cursor);
			if (findingCategory) query.set('finding_category', findingCategory);
			const path = `/api/v1/library/management/identity-preparations/${encodeURIComponent(jobId)}/findings`;
			return query.size ? `${path}?${query.toString()}` : path;
		},
		applyIdentityPreparation: (jobId: string) =>
			`/api/v1/library/management/identity-preparations/${encodeURIComponent(jobId)}/apply`,
		discardIdentityPreparation: (jobId: string) =>
			`/api/v1/library/management/identity-preparations/${encodeURIComponent(jobId)}/discard`,
		scanDiagnostics: (runId: string) => `/api/v1/library/scan-runs/${runId}/diagnostics`,
		removeAlbum: (mbid: string) => `/api/v1/library/album/${mbid}`,
		resolveTracks: () => '/api/v1/library/resolve-tracks'
	},
	libraryManagement: {
		activationPreviews: () => '/api/v1/settings/library-management/activation-previews',
		activationPreview: (jobId: string) =>
			`/api/v1/settings/library-management/activation-previews/${encodeURIComponent(jobId)}`,
		activationConfirmations: () => '/api/v1/settings/library-management/activation-confirmations',
		previews: () => '/api/v1/library/management/previews',
		tagEditor: (trackId: string) =>
			`/api/v1/library/management/tracks/${encodeURIComponent(trackId)}/tag-editor`,
		tagEditPreviews: () => '/api/v1/library/management/tag-edit-previews',
		baselineRestorePreviews: () => '/api/v1/library/management/baselines/restore-previews',
		duplicateResolutionPreviews: () => '/api/v1/library/management/duplicate-resolution-previews',
		baselinePurgeImpact: () => '/api/v1/library/management/baselines/purge-impact',
		purgeBaselines: () => '/api/v1/library/management/baselines/purge',
		recoveryDiagnostics: () => '/api/v1/library/management/recovery/diagnostics',
		resolveImportBundle: (bundleId: string) =>
			`/api/v1/library/management/recovery/import-bundles/${encodeURIComponent(bundleId)}/resolve`,
		preview: (jobId: string) => `/api/v1/library/management/previews/${encodeURIComponent(jobId)}`,
		applyPreview: (jobId: string) =>
			`/api/v1/library/management/previews/${encodeURIComponent(jobId)}/apply`,
		reissuePreview: (jobId: string) =>
			`/api/v1/library/management/previews/${encodeURIComponent(jobId)}/reissue`,
		discardPreview: (jobId: string) =>
			`/api/v1/library/management/previews/${encodeURIComponent(jobId)}/discard`,
		operations: (
			params: {
				limit?: number;
				cursor?: string;
				origin?: string;
				profileId?: string;
				rootId?: string;
				state?: string;
				mode?: string;
				createdFrom?: number;
				createdTo?: number;
			} = {}
		) => {
			const query = new URLSearchParams();
			if (params.limit !== undefined) query.set('limit', String(params.limit));
			if (params.cursor) query.set('cursor', params.cursor);
			if (params.origin) query.set('origin', params.origin);
			if (params.profileId) query.set('profile_id', params.profileId);
			if (params.rootId) query.set('root_id', params.rootId);
			if (params.state) query.set('state', params.state);
			if (params.mode) query.set('mode', params.mode);
			if (params.createdFrom !== undefined) query.set('created_from', String(params.createdFrom));
			if (params.createdTo !== undefined) query.set('created_to', String(params.createdTo));
			const path = '/api/v1/library/management/operations';
			return `${path}${query.size ? `?${query.toString()}` : ''}`;
		},
		operation: (jobId: string) =>
			`/api/v1/library/management/operations/${encodeURIComponent(jobId)}`,
		undoPreview: (jobId: string) =>
			`/api/v1/library/management/operations/${encodeURIComponent(jobId)}/undo-preview`,
		operationResults: (jobId: string, afterOrdinal?: number, limit?: number) => {
			const query = new URLSearchParams();
			if (afterOrdinal !== undefined) query.set('after_ordinal', String(afterOrdinal));
			if (limit !== undefined) query.set('limit', String(limit));
			const path = `/api/v1/library/management/operations/${encodeURIComponent(jobId)}/results`;
			return `${path}${query.size ? `?${query.toString()}` : ''}`;
		},
		previewItems: (
			jobId: string,
			params: {
				afterOrdinal?: number;
				limit?: number;
				eligibility?: string;
				reasonCode?: string;
				rootId?: string;
				artistId?: string;
				albumId?: string;
				audioFormat?: string;
				collisionClass?: string;
				hasPreservedValue?: boolean;
				hasRepresentationLoss?: boolean;
				changeKind?: string;
			} = {}
		) => {
			const query = new URLSearchParams();
			if (params.afterOrdinal !== undefined)
				query.set('after_ordinal', String(params.afterOrdinal));
			if (params.limit !== undefined) query.set('limit', String(params.limit));
			if (params.eligibility) query.set('eligibility', params.eligibility);
			if (params.reasonCode) query.set('reason_code', params.reasonCode);
			if (params.rootId) query.set('root_id', params.rootId);
			if (params.artistId) query.set('artist_id', params.artistId);
			if (params.albumId) query.set('album_id', params.albumId);
			if (params.audioFormat) query.set('audio_format', params.audioFormat);
			if (params.collisionClass) query.set('collision_class', params.collisionClass);
			if (params.hasPreservedValue) query.set('has_preserved_value', 'true');
			if (params.hasRepresentationLoss) query.set('has_representation_loss', 'true');
			if (params.changeKind) query.set('change_kind', params.changeKind);
			const path = `/api/v1/library/management/previews/${encodeURIComponent(jobId)}/items`;
			return `${path}${query.size ? `?${query.toString()}` : ''}`;
		},
		previewArtwork: (jobId: string, ordinal: number, sha256: string) =>
			`/api/v1/library/management/previews/${encodeURIComponent(jobId)}/items/${ordinal}/artwork/${encodeURIComponent(sha256)}`
	},
	cacheSync: {
		status: () => '/api/v1/cache/sync/status',
		cancel: () => '/api/v1/cache/sync/cancel'
	},
	youtube: {
		generate: () => '/api/v1/youtube/generate',
		link: (albumId: string) => `/api/v1/youtube/link/${albumId}`,
		links: () => '/api/v1/youtube/links',
		deleteLink: (albumId: string) => `/api/v1/youtube/link/${albumId}`,
		updateLink: (albumId: string) => `/api/v1/youtube/link/${albumId}`,
		manual: () => '/api/v1/youtube/manual',
		generateTrack: () => '/api/v1/youtube/generate-track',
		generateTracks: () => '/api/v1/youtube/generate-tracks',
		trackLinks: (albumId: string) => `/api/v1/youtube/track-links/${albumId}`
	},
	// profile + connections + spotify builders lived here until the profile
	// seam migration moved them onto the v3 registry (their endpoints.ts
	// files); only stragglers with out-of-scope consumers remain.
	download: {
		localTrack: (trackId: string) => `/api/v1/download/local/track/${trackId}`,
		localAlbum: (albumId: string) => `/api/v1/download/local/album/${albumId}`,
		localAlbumByMbid: (mbid: string) => `/api/v1/download/local/album/mbid/${mbid}`,
		access: () => '/api/v1/download/access'
	},
	freeMusic: {
		tasks: (all: boolean = false) => `/api/v1/free-music/tasks${all ? '?all=true' : ''}`,
		remove: (id: string) => `/api/v1/free-music/tasks/${id}`,
		clearHistory: (all: boolean = false) => `/api/v1/free-music/tasks${all ? '?all=true' : ''}`,
		cancel: (id: string) => `/api/v1/free-music/tasks/${id}/cancel`,
		retry: (id: string) => `/api/v1/free-music/tasks/${id}/retry`
	},
	dropImport: {
		uploads: () => '/api/v1/import/uploads',
		jobs: (all: boolean = false) => `/api/v1/import/jobs${all ? '?all=true' : ''}`,
		match: (itemId: number) => `/api/v1/import/items/${itemId}/match`,
		discard: (itemId: number) => `/api/v1/import/items/${itemId}/discard`
	},
	downloads: {
		activitySummary: () => '/api/v1/downloads/activity-summary',
		searchAlbum: () => '/api/v1/downloads/search/album',
		searchJob: (jobId: string) => `/api/v1/downloads/search/${jobId}`,
		pick: (jobId: string) => `/api/v1/downloads/search/${jobId}/pick`,
		dismissReview: (jobId: string) => `/api/v1/downloads/search/${jobId}/dismiss`,
		cancelSearch: (jobId: string) => `/api/v1/downloads/search/${jobId}/cancel`,
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
		stream: (taskId: string) => `/api/v1/downloads/${taskId}/stream`,
		cancel: (taskId: string) => `/api/v1/downloads/${taskId}/cancel`,
		nextSource: (taskId: string) => `/api/v1/downloads/${taskId}/next-source`,
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
		heldReverify: (id: number) => `/api/v1/downloads/held/${id}/reverify`,
		heldReverifyBulk: () => '/api/v1/downloads/held/reverify',
		heldManagementRetry: (taskId: string) => `/api/v1/downloads/held/management/${taskId}/retry`,
		heldManagementDiscard: (taskId: string) =>
			`/api/v1/downloads/held/management/${taskId}/discard`,
		heldVerdictDiscard: (taskId: string) => `/api/v1/downloads/held/verdict/${taskId}/discard`,
		heldAudio: (id: number) => `/api/v1/downloads/held/${id}/audio`,
		cutoffUnmet: () => '/api/v1/downloads/cutoff-unmet',
		upgradeAlbum: () => '/api/v1/downloads/upgrade/album',
		upgradeTrack: () => '/api/v1/downloads/upgrade/track'
	}
} as const;
