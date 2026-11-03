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

export type Track = Schemas['AlbumTrack'];

// The library's own album pages build one from a local album, which has no
// provider source or request state.
export type AlbumBasicInfo = Omit<
	Schemas['AlbumBasicInfo'],
	'requested' | 'service_status' | 'source'
> &
	Partial<Pick<Schemas['AlbumBasicInfo'], 'requested' | 'service_status'>>;

export type AlbumTracksInfo = Schemas['AlbumTracksInfo'];

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

export type PurchaseLink = components['schemas']['PurchaseLink'];

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

export type LyricLine = Schemas['LyricLine'];

export type AlbumSort = 'recent' | 'title' | 'artist';

export type AlbumView = Schemas['AlbumView'];
export type AlbumDetailView = Schemas['AlbumDetailView'];
export type ArtistView = Schemas['ArtistView'];
export type TrackView = Schemas['TrackView'];
/** A held track with its quality tier against the upgrade cutoff. */
export type LibraryStatusTrack = Schemas['LibraryStatusTrack'];

export type ReleaseTextField = Schemas['ReleaseTextField'];
export type ReleaseDraft = Schemas['ReleaseDraft'];
export type ContributionDuplicateCandidate = Schemas['DuplicateCandidate'];

export type MusicBrainzSeed = components['schemas']['MusicBrainzSeed'];

export type LibraryContribution = Schemas['ContributionRecord'];

export type ArtistSort = 'name' | 'album_count' | 'appearance_count' | 'date_added';
export type LibraryArtistScope = 'album' | 'contributors';

export type LibraryAlbumStatus = Schemas['LibraryAlbumStatus'];

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

export type RadioSeedItem = components['schemas']['RadioSeedItem'];
