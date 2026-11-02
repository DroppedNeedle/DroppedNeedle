import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type Artist = {
	title: string;
	musicbrainz_id: string;
	in_library: boolean;
	cover_url?: string | null;
	thumb_url?: string | null;
	fanart_url?: string | null;
	banner_url?: string | null;
	disambiguation?: string | null;
	type_info?: string | null;
	release_group_count?: number | null;
	listen_count?: number | null;
	score?: number;
	local_id?: string | null;
};

/**
 * Provider-lane edition pick basis (mirrors AlbumService._effective_release_id):
 * pin > owned identity > unanimous embedded tags > file count > ranked.
 */
export type EditionPickBasis = 'pin' | 'owned' | 'embedded_tags' | 'file_count' | 'ranked';

export type Album = {
	title: string;
	artist: string | null;
	year: number | null;
	musicbrainz_id: string;
	in_library: boolean;
	requested?: boolean;
	cover_url?: string | null;
	album_thumb_url?: string | null;
	album_back_url?: string | null;
	album_cdart_url?: string | null;
	album_spine_url?: string | null;
	album_3d_case_url?: string | null;
	album_3d_flat_url?: string | null;
	album_3d_face_url?: string | null;
	album_3d_thumb_url?: string | null;
	type_info?: string | null;
	disambiguation?: string | null;
	track_count?: number | null;
	listen_count?: number | null;
	score?: number;
	selected_release_mbid?: string | null;
	pick_basis?: EditionPickBasis | null;
	local_id?: string | null;
	art_missing?: boolean;
};

export type LibraryMembershipResponse = Schemas['LibraryMembershipResponse'];

export type SuggestResult = {
	type: 'artist' | 'album';
	title: string;
	artist?: string | null;
	year?: number | null;
	musicbrainz_id: string;
	in_library: boolean;
	requested?: boolean;
	disambiguation?: string | null;
	score: number;
	local_id?: string | null;
};

export type SearchRemoteStatus = 'ok' | 'partial' | 'timeout' | 'error' | 'stale';

export type SearchBucketResponse<T extends Artist | Album> = {
	bucket: 'artists' | 'albums';
	limit: number;
	offset: number;
	results: T[];
	top_result?: T | null;
	status: SearchRemoteStatus;
};

export type SearchSuggestResponse = {
	results: SuggestResult[];
	remote_status: SearchRemoteStatus;
};

/** The provider behind an enrichment batch; `null` when none answered. */
export type EnrichmentSource = components['schemas']['ListenSource'] | null;

export type ArtistEnrichment = components['schemas']['ArtistEnrichment'];

export type AlbumEnrichment = components['schemas']['AlbumEnrichment'];

export type ArtistEnrichmentRequest = {
	musicbrainz_id: string;
	name: string;
};

export type AlbumEnrichmentRequest = components['schemas']['AlbumEnrichmentRequest'];

export type TopSong = components['schemas']['TopSong'];

export type SimilarAlbumsResponse = components['schemas']['SimilarAlbumsResponse'];

export type MoreByArtistResponse = components['schemas']['MoreByArtistResponse'];

export type ReleaseGroup = {
	id: string;
	title: string;
	type?: string;
	year?: number;
	first_release_date?: string;
	in_library: boolean;
	requested?: boolean;
};

export type ExternalLink = {
	type: string;
	url: string;
	label: string;
	category?: string;
};

/** TheAudioDB artist images, flat as the artist page reads them. */
export type ArtistImageFields = {
	fanart_url?: string | null;
	banner_url?: string | null;
	thumb_url?: string | null;
	fanart_url_2?: string | null;
	fanart_url_3?: string | null;
	fanart_url_4?: string | null;
	wide_thumb_url?: string | null;
	logo_url?: string | null;
	clearart_url?: string | null;
	cutout_url?: string | null;
};

export type ArtistInfoBasic = ArtistImageFields & {
	name: string;
	musicbrainz_id: string;
	disambiguation?: string | null;
	type?: string | null;
	country?: string | null;
	life_span?: {
		begin?: string | null;
		end?: string | null;
		ended?: boolean | null;
	} | null;
	tags: string[];
	aliases: string[];
	external_links: ExternalLink[];
	in_library: boolean;
	appears_in_library: boolean;
	// per-user follow state; artist page reads it from the dedicated /follow query
	followed?: boolean;
	auto_download?: boolean;
	auto_download_state?: components['schemas']['AutoDownloadState'];
	release_group_count?: number;
	service_status?: Record<string, string> | null;
};

export type ArtistInfoExtended = {
	description?: string | null;
	image?: string | null;
};

export type ArtistInfo = ArtistInfoBasic & ArtistInfoExtended;

export type ArtistReleases = {
	albums: ReleaseGroup[];
	singles: ReleaseGroup[];
	eps: ReleaseGroup[];
	offset: number;
	limit: number;
	returned_count: number;
	next_offset: number | null;
	has_more: boolean;
	source_total_count?: number | null;
	warming?: boolean;
	service_status?: Record<string, string> | null;
};

export type UserPreferences = {
	primary_types: string[];
	secondary_types: string[];
};

export type ReleaseTypeOption = {
	id: string;
	title: string;
	description: string;
};

export type Track = {
	position: number;
	disc_number?: number | null;
	title: string;
	length?: number | null;
	recording_id?: string | null;
	release_track_id?: string | null;
	media_format?: string | null;
};

export type AlbumBasicInfo = {
	title: string;
	musicbrainz_id: string;
	artist_name: string;
	artist_id: string;
	release_date?: string | null;
	year?: number | null;
	type?: string | null;
	disambiguation?: string | null;
	in_library: boolean;
	requested?: boolean;
	cover_url?: string | null;
	album_thumb_url?: string | null;
	service_status?: Record<string, string> | null;
};

export type AlbumTracksInfo = {
	tracks: Track[];
	total_tracks: number;
	total_length?: number | null;
	label?: string | null;
	barcode?: string | null;
	country?: string | null;
	selected_release_mbid?: string | null;
};

export type OIDCConnectionSettings = components['schemas']['OidcConnection'];

export type ServiceHealthItem = components['schemas']['ServiceHealthItem'];

export type SystemHealthResponse = components['schemas']['SystemHealthResponse'];

export type ResolvedTrack = {
	release_group_mbid?: string | null;
	disc_number?: number | null;
	track_number?: number | null;
	source?: string | null;
	track_source_id?: string | null;
	stream_url?: string | null;
	format?: string | null;
	duration?: number | null;
};

export type DiscoverActivity = components['schemas']['DiscoverActivityRequest'];

export type YouTubeQuotaStatus = components['schemas']['YouTubeQuotaStatus'];

export type TrackCacheCheckItem = {
	artist: string;
	track: string;
	cached: boolean;
};

export type YouTubeLink = components['schemas']['YouTubeLink'];

export type YouTubeLinkResponse = components['schemas']['YouTubeLinkResponse'];

export type YouTubeTrackLink = components['schemas']['YouTubeTrackLink'];

export type YouTubeTrackLinkResponse = components['schemas']['YouTubeTrackLinkResponse'];

export type YouTubeTrackLinkBatchResponse = components['schemas']['YouTubeTrackLinkBatchResponse'];

export type NavidromePlaylistSyncResult = components['schemas']['PlaylistSyncResult'];

export type BrowseHeroCard = {
	label: string;
	value: number | null;
	href: string;
	subtitle?: string;
	colorScheme: 'primary' | 'secondary' | 'accent';
	icon: 'disc' | 'users' | 'music';
};

export type ArtistIndexArtist = {
	id: string;
	name: string;
	image_url?: string | null;
	album_count?: number;
	musicbrainz_id?: string | null;
};

export type ArtistIndexEntry = {
	name: string;
	artists: ArtistIndexArtist[];
};

export type LocalTrackInfo = {
	track_file_id: string;
	title: string;
	track_number: number;
	disc_number?: number | null;
	duration_seconds?: number | null;
	size_bytes: number;
	format: string;
	bitrate?: number | null;
	date_added?: string | null;
};

export type LocalAlbumMatch = {
	found: boolean;
	musicbrainz_id?: string | null;
	tracks: LocalTrackInfo[];
	total_size_bytes: number;
	primary_format?: string | null;
	download_allowed?: boolean;
};

export type LocalAlbumSummary = {
	musicbrainz_id: string;
	name: string;
	artist_name: string;
	artist_mbid?: string | null;
	year?: number | null;
	track_count: number;
	total_size_bytes: number;
	primary_format?: string | null;
	cover_url?: string | null;
	date_added?: string | null;
	download_allowed?: boolean;
};

export type CrateReason = 'recent' | 'rediscover' | 'surprise' | 'same_era';

export type CrateTrack = {
	track_file_id: string;
	title: string;
	album_name: string;
	artist_name: string;
	album_mbid?: string | null;
	cover_url?: string | null;
	format?: string;
	year?: number | null;
	duration_seconds?: number | null;
	reason: CrateReason;
};

export type LastFmConnectionSettingsResponse = components['schemas']['LastFmSettings'];

// mirrors backend api/v1/schemas/settings.py (GetItSettings)
export type GetItSettings = components['schemas']['GetIt'];

// How editions are ranked when the files do not settle it.
export type EditionPreferences = components['schemas']['EditionPreferences'];

export type PurchaseLink = components['schemas']['PurchaseLink'];

// mirrors backend api/v1/schemas/settings.py (EventsSettings)
export type EventsSettings = components['schemas']['EventsSettings'];

export type WrappedSettingsResponse = components['schemas']['WrappedSettings'];

export type ScrobbleSettings = {
	scrobble_to_lastfm: boolean;
	scrobble_to_listenbrainz: boolean;
	navidrome_handles_external_scrobbles: boolean;
};

export type NowPlayingSubmission = {
	track_name: string;
	artist_name: string;
	album_name: string;
	duration_ms: number;
	mbid?: string;
	source?: string;
};

export type ScrobbleSubmission = {
	track_name: string;
	artist_name: string;
	album_name: string;
	timestamp: number;
	duration_ms: number;
	mbid?: string;
	source?: string;
};

export type LastFmArtistEnrichment = components['schemas']['LastFmArtistEnrichment'];
export type LastFmAlbumEnrichment = components['schemas']['LastFmAlbumEnrichment'];

export type SourcePlaylistSummary = {
	id: string;
	name: string;
	track_count: number;
	duration_seconds: number;
	cover_url: string;
	is_smart?: boolean;
	is_imported?: boolean;
	owner?: string;
	is_public?: boolean;
	updated_at?: string;
	created_at?: string;
};

export type SourcePlaylistSource = 'jellyfin' | 'navidrome' | 'plex';

export type SourcePlaylistCollection = {
	account_mode: 'linked' | 'shared';
	account_label: string;
	playlists: SourcePlaylistSummary[];
};

export type SourcePlaylistTrack = {
	id: string;
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id: string;
	artist_id?: string;
	plex_rating_key?: string;
	duration_seconds: number;
	track_number: number;
	disc_number: number;
	cover_url: string;
};

export type SourcePlaylistDetail = {
	id: string;
	name: string;
	track_count: number;
	duration_seconds: number;
	cover_url: string;
	is_smart?: boolean;
	updated_at?: string;
	created_at?: string;
	tracks: SourcePlaylistTrack[];
};

export type NowPlayingSession = {
	id: string;
	user_name: string;
	track_name: string;
	artist_name: string;
	album_name: string;
	cover_url: string;
	device_name: string;
	is_paused: boolean;
	// upstream servers use 'jellyfin'|'navidrome'|'plex'; the web player adds
	// 'local'|'youtube'; connected apps report their client name (e.g. 'finamp')
	source?: string;
	progress_ms?: number;
	duration_ms?: number;
	audio_codec?: string;
	bitrate?: number;
	_isLocal?: boolean;
	// owner chose 'track_hidden': identity + progress only, song fields blank
	redacted?: boolean;
};

export type LyricLine = Schemas['LyricLine'];

export type AlbumSort = 'recent' | 'title' | 'artist';

export type AlbumIdentityState = 'local_only' | 'release_group_linked' | 'release_linked';

export type ArtistIdentityState = 'local_only' | 'musicbrainz_linked';

export interface TargetNativeTrack {
	id: string;
	title: string;
	album_id: string;
	album_title: string;
	artist_id: string;
	artist_name: string;
	album_artist_id: string;
	album_artist_name: string;
	musicbrainz_recording_id: string | null;
	musicbrainz_release_group_id: string | null;
	musicbrainz_artist_id: string | null;
	musicbrainz_album_artist_id: string | null;
	disc_number: number;
	track_number: number;
	year: number | null;
	genre: string | null;
	duration_seconds: number;
	format: string;
	bit_rate: number | null;
	sample_rate: number | null;
	bit_depth: number | null;
	channels: number | null;
	file_size_bytes: number;
	date_added: number | null;
	cover_available: boolean;
	/** The server knows the album's cover routes answer 404: ask for no cover. */
	art_missing?: boolean;
	current_tier: string | null;
	below_cutoff: boolean;
	/** Whether the caller favorited it, where the read says. */
	favorite?: boolean;
}

export type NativeTrackListItem = TargetNativeTrack;

export interface LibraryAlbumSummary {
	id: string;
	title: string;
	artist_name: string;
	artist_id: string;
	musicbrainz_release_group_id: string | null;
	musicbrainz_release_id: string | null;
	musicbrainz_artist_id: string | null;
	album_identity_state: AlbumIdentityState;
	track_count: number;
	total_duration_seconds: number | null;
	total_size_bytes: number;
	format: string | null;
	year: number | null;
	is_compilation: boolean;
	cover_available: boolean;
	/** The server knows its cover routes answer 404: ask for no cover. */
	art_missing?: boolean;
	date_added: number | null;
	contribution_id: string | null;
	contribution_state: ContributionState | null;
	/** Whether the caller favorited it, where the read says. */
	favorite?: boolean;
}

export interface LibraryAlbumDetail extends LibraryAlbumSummary {
	// Set when library management leaves the album's files alone.
	management_hold?: components['schemas']['ManagementHoldView'] | null;
	row_revision: number;
	download_allowed?: boolean;
	/** The album's pending identification review. */
	review_id: string | null;
}

export type ContributionState = Schemas['ContributionState'];
export type ReleaseTextField = Schemas['ReleaseTextField'];
export type ReleaseDraft = Schemas['ReleaseDraft'];
export type ContributionDuplicateCandidate = Schemas['DuplicateCandidate'];

export type DiscogsReleaseCandidate = components['schemas']['DiscogsReleaseCandidate'];

export type MusicBrainzSeedField = components['schemas']['MusicBrainzSeedField'];

export type MusicBrainzSeed = components['schemas']['MusicBrainzSeed'];

export type LibraryContribution = Schemas['ContributionRecord'];

export interface NativeAlbumsResponse {
	items: LibraryAlbumSummary[];
	total: number;
}

export type ArtistSort = 'name' | 'album_count' | 'appearance_count' | 'date_added';
export type LibraryArtistScope = 'album' | 'contributors';
export type LibraryArtistRelationship = 'album_artist' | 'contributor' | 'both';

export interface LibraryArtistSummary {
	id: string;
	name: string;
	musicbrainz_artist_id: string | null;
	artist_identity_state: ArtistIdentityState;
	album_count: number;
	track_count: number;
	appearance_release_count: number;
	appearance_track_count: number;
	library_relationship: LibraryArtistRelationship;
	date_added: number | null;
	/** The server knows its picture route answers 404: ask for no picture. */
	art_missing?: boolean;
	/** Whether the caller favorited it, where the read says. */
	favorite?: boolean;
}

export interface NativeArtistsResponse {
	items: LibraryArtistSummary[];
	total: number;
	album_artist_total: number;
	contributor_total: number;
}

export interface LibraryArtistAppearance {
	album: LibraryAlbumSummary;
	tracks: NativeTrackListItem[];
}

export interface LibraryArtistAppearancesResponse {
	items: LibraryArtistAppearance[];
	total: number;
	total_tracks: number;
	/** The cursor of the page after this one; null on the last page. */
	next: string | null;
}

export type LibraryFileMeta = TargetNativeTrack;

// The contract's status, its tracks in the native track model.
export type LibraryAlbumStatus = Omit<Schemas['LibraryAlbumStatus'], 'tracks'> & {
	tracks: TargetNativeTrack[];
};

export type AlbumEditionItem = components['schemas']['AlbumEditionItem'];
export type AlbumEditionsResponse = components['schemas']['AlbumEditionsResponse'];

export type LibraryStats = Schemas['StatsView'];

export type ScanFrequency = Schemas['ScanFrequency'];

export type LibraryScanSchedule = Schemas['LibraryScanSchedule'];

export type DownloadStatus = components['schemas']['TaskStatus'];

export type DownloadTask = components['schemas']['DownloadTaskResponse'];

/** Why a download, held file or upload sits where it does. */
export type Reason = components['schemas']['Reason'];

export type DownloadListResponse = components['schemas']['DownloadListResponse'];

export type DownloadActivitySummary = components['schemas']['DownloadActivitySummaryResponse'];

/** One download or upload whose files wait for a decision. */
export type HeldEntry = components['schemas']['HeldEntryResponse'];

export type HeldFile = components['schemas']['HeldFileResponse'];

export type HeldFileAction = components['schemas']['HeldFileAction'];

/** A held file with the entry it belongs to, as an album page's track row shows it. */
export interface HeldFileInEntry {
	entry: HeldEntry;
	file: HeldFile;
}

export type HeldActionResult = components['schemas']['HeldActionResponse'];

export type HeldRecheckAllResult = components['schemas']['HeldRecheckAllResponse'];

// Connect Apps (inbound OpenSubsonic + Jellyfin compatibility)
export type ConnectAppsSettings = components['schemas']['ConnectApps'];
export type MyConnectApps = components['schemas']['MyConnectApps'];

export type AppPasswordView = components['schemas']['AppPasswordView'];

// Admin oversight roster: every user's active app-passwords (never a secret).
export type AdminAppPasswordView = components['schemas']['AdminAppPasswordView'];

export type SectionPrefItem = components['schemas']['SectionPrefItem'];

export interface SectionPrefsResponse {
	pages: Record<string, SectionPrefItem[]>;
}

export interface SectionPrefsUpdate {
	page: 'home' | 'discover' | 'sidebar';
	sections: { key: string; enabled: boolean }[];
}

export type RadioSeedItem = components['schemas']['RadioSeedItem'];
