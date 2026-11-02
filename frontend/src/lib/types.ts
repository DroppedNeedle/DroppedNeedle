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

export type Album = {
	title: string;
	artist: string | null;
	year: number | null;
	musicbrainz_id: string;
	in_library: boolean;
	requested?: boolean;
	cover_url?: string | null;
	album_thumb_url?: string | null;
	type_info?: string | null;
	disambiguation?: string | null;
	track_count?: number | null;
	listen_count?: number | null;
	score?: number;
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

export type SearchRemoteStatus = Schemas['SearchRemoteStatus'];

/** The provider behind an enrichment batch; `null` when none answered. */
export type EnrichmentSource = components['schemas']['ListenSource'] | null;

export type TopSong = components['schemas']['TopSong'];

export type SimilarAlbumsResponse = components['schemas']['SimilarAlbumsResponse'];

export type MoreByArtistResponse = components['schemas']['MoreByArtistResponse'];

export type ReleaseGroup = Omit<Schemas['ReleaseItem'], 'title'> & { title: string };

export type ExternalLink = Schemas['ExternalLink'];

export type ArtistInfoBasic = Omit<
	Schemas['ArtistInfo'],
	'albums' | 'singles' | 'eps' | 'source' | 'images'
> &
	Schemas['ArtistImages'];

export type ArtistInfo = ArtistInfoBasic & {
	description?: string | null;
	image?: string | null;
};

export type ArtistReleases = Omit<
	Schemas['ArtistReleases'],
	'albums' | 'singles' | 'eps' | 'source'
> & {
	albums: ReleaseGroup[];
	singles: ReleaseGroup[];
	eps: ReleaseGroup[];
};

export type UserPreferences = Schemas['UserPreferences'];

export type ReleaseTypeOption = {
	id: string;
	title: string;
	description: string;
};

export type Track = Schemas['AlbumTrack'];

// The library's own album pages build one from a local album, which has no
// provider source or request state.
export type AlbumBasicInfo = Omit<
	Schemas['AlbumBasicInfo'],
	'requested' | 'service_status' | 'source'
> &
	Partial<Pick<Schemas['AlbumBasicInfo'], 'requested' | 'service_status'>>;

export type AlbumTracksInfo = Schemas['AlbumTracksInfo'];

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

export type YouTubeLink = components['schemas']['YouTubeLink'];

export type YouTubeTrackLink = components['schemas']['YouTubeTrackLink'];

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
	/** What removing this file sends back. Absent where the view offers no removal. */
	row_revision?: components['schemas']['TrackView']['row_revision'];
	title: string;
	track_number: number;
	disc_number?: number | null;
	duration_seconds?: number | null;
	size_bytes: number;
	format: string;
	bitrate?: number | null;
};

export type LocalAlbumMatch = {
	found: boolean;
	musicbrainz_id?: string | null;
	tracks: LocalTrackInfo[];
	total_size_bytes: number;
	primary_format?: string | null;
	download_allowed?: boolean;
};

export type CrateReason = 'recent' | 'rediscover' | 'surprise' | 'same_era';

export type CrateTrack = {
	track_file_id: string;
	title: string;
	album_name: string;
	artist_name: string;
	album_id: string;
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

export interface NativeTrackListItem {
	/** What removing this file sends back. Only the album status view carries it. */
	row_revision?: number;
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
	row_revision: components['schemas']['AlbumDetailView']['row_revision'];
	download_allowed?: boolean;
	/** The album's pending identification review. */
	review_id: string | null;
}

export type ContributionState = Schemas['ContributionState'];
export type ReleaseTextField = Schemas['ReleaseTextField'];
export type ReleaseDraft = Schemas['ReleaseDraft'];
export type ContributionDuplicateCandidate = Schemas['DuplicateCandidate'];

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

export interface LibraryArtistAppearancesResponse {
	items: { album: LibraryAlbumSummary; tracks: NativeTrackListItem[] }[];
	total: number;
	total_tracks: number;
	/** The cursor of the page after this one; null on the last page. */
	next: string | null;
}

export type LibraryFileMeta = NativeTrackListItem;

// The contract's status, its tracks in the native track model.
export type LibraryAlbumStatus = Omit<Schemas['LibraryAlbumStatus'], 'tracks'> & {
	tracks: NativeTrackListItem[];
};

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
