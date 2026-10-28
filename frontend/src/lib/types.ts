import type { components } from '$lib/api/v3/openapi';
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

/**
 * Native-lane pick basis (TargetNativeLibraryService._display_pick): per-copy
 * pin, owned identity, or unanimous tags only - there is no file-count/ranked
 * fallback on that path, so unproven albums without tags get no pick.
 */
export type NativePickBasis = 'pin' | 'owned' | 'embedded_tags';

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
	cover_available?: boolean;
};

export interface LibraryMembershipResponse {
	owned_ids: string[];
	requested_ids: string[];
}

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

export type CrateResponse = { items: CrateTrack[] };

export type LocalSearchResponse = {
	albums: LocalAlbumSummary[];
	tracks: CrateTrack[];
};

export type DecadeShelf = {
	decade: number;
	label: string;
	album_count: number;
	albums: LocalAlbumSummary[];
};

export type DecadesResponse = { items: DecadeShelf[] };

export type LocalPaginatedResponse = {
	items: LocalAlbumSummary[];
	total: number;
	offset: number;
	limit: number;
};

export type FormatInfo = {
	count: number;
	size_bytes: number;
	size_human: string;
};

export type LocalStorageStats = {
	total_tracks: number;
	total_albums: number;
	total_artists: number;
	total_size_bytes: number;
	total_size_human: string;
	disk_free_bytes: number;
	disk_free_human: string;
	format_breakdown: Record<string, FormatInfo>;
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

export type LyricLine = {
	text: string;
	start_seconds: number | null;
};

export type LocalLyricsResponse = {
	text: string;
	is_synced: boolean;
	lines: LyricLine[];
};

export type AlbumSort = 'recent' | 'title' | 'artist';

export type TrackSort = 'recent' | 'title' | 'artist' | 'album';

export type AlbumIdentityState =
	'local_only' | 'release_group_linked' | 'release_linked' | 'custom_edition';

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
	current_tier: string | null;
	below_cutoff: boolean;
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
	release_type: string | null;
	cover_available: boolean;
	date_added: number | null;
	sort_name: string | null;
	original_release_date: string | null;
	contribution_id: string | null;
	contribution_state: ContributionState | null;
}

export interface LibraryAlbumDetail extends LibraryAlbumSummary {
	// Set when library management leaves the album's files alone.
	management_hold?: components['schemas']['ManagementHoldView'] | null;
	row_revision: number;
	input_revision: string;
	download_allowed?: boolean;
	identification_status:
		| 'identified'
		| 'needs_review'
		| 'keep_tagged'
		| 'local_metadata'
		| 'manual_identity_needs_review';
	review_id: string | null;
	review_revision: number | null;
	management_identity_readiness:
		| 'not_applicable'
		| 'exact_release_required'
		| 'track_mapping_required'
		| 'custom_manifest_stale'
		| 'ready';
	mapped_track_count: number;
	management_identity_kind: 'exact_release' | 'custom_edition' | null;
	custom_manifest_id: string | null;
	custom_manifest_version: number | null;
	custom_manifest_track_count: number;
	custom_manifest_recognized_track_count: number;
	custom_manifest_stale: boolean;
	active_edition_conversion: {
		job_id: string;
		release_mbid: string;
		state: 'preflight' | 'acquiring' | 'ready' | 'needs_recheck';
		kept_count: number;
		acquire_count: number;
		staged_count: number;
		failed_count: number;
		recycle_count: number;
		row_revision: number;
		final_preview_job_id: string | null;
	} | null;
	display_release_mbid: string | null;
	pick_basis: NativePickBasis | null;
}

export type ContributionState =
	'draft' | 'ready' | 'seeded' | 'verifying' | 'linked' | 'needs_review' | 'stale' | 'cancelled';

export type ContributionNextAction =
	| 'edit_draft'
	| 'refresh_discogs'
	| 'run_duplicate_check'
	| 'attach_existing'
	| 'seed_musicbrainz'
	| 'retry_verification'
	| 'rebuild'
	| 'cancel';

export type ContributionFieldSource = 'local' | 'discogs' | 'entered_here';

export interface ReleaseTextField {
	value: string | null;
	source: ContributionFieldSource;
}

export interface ReleaseTrackSnapshot {
	local_track_id: string;
	disc_number: number;
	track_number: number;
	title: string;
	artist_name: string | null;
	duration_seconds: number | null;
	duration_reliable: boolean;
}

export interface ReleaseMediumSnapshot {
	position: number;
	title: string | null;
	tracks: ReleaseTrackSnapshot[];
}

export interface LocalReleaseSnapshot {
	schema_version: number;
	local_album_id: string;
	local_artist_id: string;
	album_row_revision: number;
	input_revision: string;
	title: string;
	album_artist_name: string;
	artist_kind: string;
	musicbrainz_artist_id: string | null;
	musicbrainz_release_group_id: string | null;
	musicbrainz_release_id: string | null;
	release_date: string | null;
	year: number | null;
	is_compilation: boolean;
	captured_at: number;
	media: ReleaseMediumSnapshot[];
}

export interface ReleaseTrackDraft {
	local_track_id: string;
	disc_number: number;
	track_number: number;
	title: ReleaseTextField;
	artist_name: ReleaseTextField;
	duration_seconds: number | null;
}

export interface ReleaseMediumDraft {
	position: number;
	title: ReleaseTextField;
	format: ReleaseTextField;
	tracks: ReleaseTrackDraft[];
}

export interface ReleaseDraft {
	schema_version: number;
	title: ReleaseTextField;
	artist_credit: ReleaseTextField;
	release_date: ReleaseTextField;
	country: ReleaseTextField;
	label: ReleaseTextField;
	catalogue_number: ReleaseTextField;
	barcode: ReleaseTextField;
	packaging: ReleaseTextField;
	media: ReleaseMediumDraft[];
}

export interface ContributionSourceReference {
	provider: string;
	entity_type: string;
	external_id: string;
	canonical_url: string;
	fetched_at: number | null;
}

export interface ContributionTrackAlignment {
	local_track_id: string;
	provider_position: string | null;
	classification: 'exact' | 'partial' | 'conflicting' | 'unmatched';
}

export interface ContributionSourceSelection {
	schema_version: number;
	sources: ContributionSourceReference[];
	alignments: ContributionTrackAlignment[];
}

export type DiscogsReleaseCandidate = components['schemas']['DiscogsReleaseCandidate'];

export interface DiscogsArtistCredit {
	name: string;
	credited_name: string | null;
	join_phrase: string;
	artist_id: string | null;
	canonical_url: string | null;
}

export interface DiscogsLabel {
	name: string;
	catalogue_number: string | null;
	label_id: string | null;
	canonical_url: string | null;
}

export interface DiscogsIdentifier {
	type: string;
	value: string;
	description: string | null;
}

export interface DiscogsFormat {
	name: string;
	quantity: number | null;
	descriptions: string[];
	text: string | null;
}

export interface DiscogsTrack {
	source_position: string | null;
	number: number | null;
	title: string;
	duration_seconds: number | null;
	heading: boolean;
	artists: DiscogsArtistCredit[];
}

export interface DiscogsMedium {
	position: number;
	title: string | null;
	format: string | null;
	tracks: DiscogsTrack[];
}

export interface DiscogsRelease {
	release_id: string;
	master_id: string | null;
	canonical_release_url: string;
	canonical_master_url: string | null;
	title: string;
	artist_name: string;
	artists: DiscogsArtistCredit[];
	released_date: string | null;
	year: number | null;
	country: string | null;
	labels: DiscogsLabel[];
	identifiers: DiscogsIdentifier[];
	barcode: string | null;
	formats: DiscogsFormat[];
	media: DiscogsMedium[];
	source_fetched_at: number;
}

export interface DiscogsSourceView {
	release: DiscogsRelease | null;
	expired: boolean;
	expires_at: number | null;
}

export interface ContributionDuplicateCandidate {
	release_mbid: string | null;
	release_group_mbid: string | null;
	title: string;
	artist_name: string;
	evidence_kind: 'exact_discogs_url' | 'release_group' | 'barcode' | 'similar';
	exact: boolean;
	differences: string[];
}

export interface ContributionDuplicateResult {
	schema_version: number;
	checked_at: number;
	input_revision: string;
	candidates: ContributionDuplicateCandidate[];
	different_edition_confirmed: boolean;
}

export type MusicBrainzSeedField = components['schemas']['MusicBrainzSeedField'];

export type MusicBrainzSeed = components['schemas']['MusicBrainzSeed'];

export interface ContributionValidationIssue {
	code: string;
	field: string;
	message: string;
}

export interface LibraryContribution {
	id: string;
	local_album_id: string;
	created_by_user_id: string | null;
	updated_by_user_id: string | null;
	state: ContributionState;
	album_row_revision: number;
	input_revision: string;
	local_snapshot: LocalReleaseSnapshot;
	draft: ReleaseDraft;
	source_selection: ContributionSourceSelection;
	provider_snapshot_expires_at: number | null;
	discogs_source: DiscogsSourceView | null;
	duplicate_result: ContributionDuplicateResult | null;
	duplicate_checked_at: number | null;
	result_release_mbid: string | null;
	result_source: 'callback' | 'manual' | null;
	result_received_at: number | null;
	seeded_at: number | null;
	terminal_at: number | null;
	created_at: number;
	updated_at: number;
	row_revision: number;
	input_is_current: boolean;
	validation: ContributionValidationIssue[];
	next_actions: ContributionNextAction[];
	/** Why a needs-review contribution stopped: code, sentence, next step. */
	review_reason?: { code: string; message: string; action: string } | null;
}

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
	row_revision: number;
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

export interface LibraryTrack {
	id: string;
	recording_mbid: string | null;
	disc_number: number;
	track_number: number;
	track_title: string;
	artist_name: string | null;
	file_path: string;
	file_format: string | null;
	bit_rate: number | null;
	sample_rate: number | null;
	bit_depth: number | null;
	duration_seconds: number | null;
	file_size_bytes: number;
	current_tier: string | null;
	below_cutoff: boolean;
}

export type LibraryFileMeta = TargetNativeTrack;

export interface LibraryAlbumStatus {
	in_library: boolean;
	album_id: string;
	track_count: number;
	tracks: TargetNativeTrack[];
	// Coverage vs the release's MB tracklist: expected_tracks === 0 means the
	// tracklist was unavailable and the UI falls back to the presence-only reading.
	expected_tracks?: number;
	covered_tracks?: number;
	matched_file_ids?: string[];
	// held files that match NONE of the album's expected tracks ("doesn't match")
	orphans?: LibraryTrack[];
}

export interface AlbumRemoveResponse {
	success: boolean;
	album_mbid: string;
	removed_mbids: string[];
	artist_removed: boolean;
	artist_name: string | null;
}

export interface TargetCatalogRemovalResponse {
	success: boolean;
	id: string;
	removed_track_ids: string[];
}

export type AlbumEditionItem = components['schemas']['AlbumEditionItem'];
export type AlbumEditionsResponse = components['schemas']['AlbumEditionsResponse'];

export interface LibraryStats {
	total_albums: number;
	total_artists: number;
	total_tracks: number;
	total_size_bytes: number;
	format_breakdown: Record<string, number>;
	review_count: number;
	local_only_count: number;
	last_scan_at: number | null;
}

export type ScanFrequency =
	'manual' | '5min' | '10min' | '30min' | '1hr' | '6hr' | '12hr' | '24hr' | '3d' | '7d' | 'daily';

export interface LibraryScanSchedule {
	scan_frequency: ScanFrequency;
	/** HH:MM (server-local) used when scan_frequency is 'daily'. */
	daily_scan_time: string;
	/** Server timezone label for the daily-time caption; present on reads only. */
	server_timezone?: string;
	/** When the last scan finished, unix seconds, from the scan history; reads only. */
	last_scan_at?: number | null;
}

export interface LibraryActionResponse {
	status: string;
	message: string;
}

// mirrors backend api/v1/schemas/common.py StatusMessageResponse
export interface StatusMessageResponse {
	status: string;
	message: string;
}

// Hand-mirrors backend PluginRouteRequest/PluginRouteResponse
// (infrastructure/plugins/protocols.py). Typed transport for one /ext/ call.
export interface PluginRouteRequest {
	method: string;
	subpath: string;
	query: Record<string, string>;
	body: unknown;
}
export interface PluginRouteResponse {
	status: number;
	body: unknown;
}

// Envelope for the admin-only plugin UI bundle (panel.js) RPC channel.
export interface PluginUiRpcRequest {
	request_id: string;
	method: string;
	params: Record<string, unknown>;
}
export interface PluginUiRpcResponse {
	request_id: string;
	ok: boolean;
	result: unknown;
	error: string | null;
}

export type DownloadStatus = components['schemas']['TaskStatus'];

// The source status still reads the quality projection and the fallback time,
// which the server does not send; they stay optional until it does or the
// screen stops reading them.
export type DownloadTask = components['schemas']['DownloadTaskResponse'] & {
	preferred_quality_fallback_at?: number | null;
	quality_snapshot_summary?: string | null;
	quality_preference_step?: number | null;
	quality_certainty?: string | null;
	quality_provenance?: string | null;
	manual_quality_override?: boolean;
};

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
