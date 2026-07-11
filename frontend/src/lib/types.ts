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
};

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
};

export type EnrichmentSource = 'listenbrainz' | 'lastfm' | 'none';

export type ArtistEnrichment = {
	musicbrainz_id: string;
	release_group_count?: number | null;
	listen_count?: number | null;
};

export type AlbumEnrichment = {
	musicbrainz_id: string;
	track_count?: number | null;
	listen_count?: number | null;
};

export type ArtistEnrichmentRequest = {
	musicbrainz_id: string;
	name: string;
};

export type AlbumEnrichmentRequest = {
	musicbrainz_id: string;
	artist_name: string;
	album_name: string;
};

export type EnrichmentResponse = {
	artists: ArtistEnrichment[];
	albums: AlbumEnrichment[];
	source: EnrichmentSource;
};

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

export type ArtistInfoBasic = {
	name: string;
	musicbrainz_id: string;
	disambiguation?: string | null;
	type?: string | null;
	country?: string | null;
	life_span?: {
		begin?: string | null;
		end?: string | null;
		ended?: boolean;
	} | null;
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
	tags: string[];
	aliases: string[];
	external_links: ExternalLink[];
	in_library: boolean;
	// per-user follow state; artist page reads it from the dedicated /follow query
	followed?: boolean;
	auto_download?: boolean;
	auto_download_state?: 'none' | 'pending' | 'approved' | 'rejected' | 'revoked';
	release_group_count?: number;
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
	source_total_count: number | null;
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
};

export type AlbumTracksInfo = {
	tracks: Track[];
	total_tracks: number;
	total_length?: number | null;
	label?: string | null;
	barcode?: string | null;
	country?: string | null;
};

export type JellyfinConnectionSettings = {
	jellyfin_url: string;
	api_key: string;
	user_id: string;
	enabled: boolean;
	login_enabled: boolean;
};

export type OIDCConnectionSettings = {
	enabled: boolean;
	issuer: string;
	client_id: string;
	client_secret: string;
	scopes: string;
	redirect_uri: string;
};

export type HomeArtist = {
	mbid: string | null;
	name: string;
	image_url: string | null;
	listen_count: number | null;
	in_library: boolean;
};

export type HomeAlbum = {
	mbid: string | null;
	name: string;
	artist_name: string | null;
	artist_mbid: string | null;
	image_url: string | null;
	release_date: string | null;
	listen_count: number | null;
	in_library: boolean;
	requested?: boolean;
};

export type HomeTrack = {
	mbid: string | null;
	name: string;
	artist_name: string | null;
	artist_mbid: string | null;
	album_name: string | null;
	listen_count: number | null;
	listened_at: string | null;
	image_url?: string | null;
};

export type HomeGenre = {
	name: string;
	listen_count: number | null;
	artist_count: number | null;
	artist_mbid: string | null;
};

export type HomeSection = {
	title: string;
	type: 'artists' | 'albums' | 'tracks' | 'genres';
	items: (HomeArtist | HomeAlbum | HomeTrack | HomeGenre)[];
	source: string | null;
	fallback_message: string | null;
	connect_service: string | null;
	radio_seed_type?: string | null;
	radio_seed_id?: string | null;
};

export type ServicePrompt = {
	service: string;
	title: string;
	description: string;
	icon: string;
	color: string;
	features: string[];
};

export type ServiceHealthItem = {
	service: string;
	capability: string;
	severity: string;
	message: string;
	fallback?: string | null;
	degraded_seconds?: number;
};

export type SystemHealthResponse = {
	degraded: ServiceHealthItem[];
};

export type HomeResponse = {
	refreshing?: boolean;
	recently_added: HomeSection | null;
	library_artists: HomeSection | null;
	library_albums: HomeSection | null;
	recommended_artists: HomeSection | null;
	trending_artists: HomeSection | null;
	popular_albums: HomeSection | null;
	recently_played: HomeSection | null;
	top_genres: HomeSection | null;
	genre_list: HomeSection | null;
	fresh_releases: HomeSection | null;
	favorite_artists: HomeSection | null;
	your_top_albums: HomeSection | null;
	weekly_exploration: WeeklyExplorationSection | null;
	service_prompts: ServicePrompt[];
	integration_status: Record<string, boolean>;
	genre_artists: Record<string, string | null>;
	genre_artist_images: Record<string, string | null>;
	discover_preview: DiscoverPreview | null;
};

export type DiscoverPreview = {
	seed_artist: string;
	seed_artist_mbid: string;
	items: HomeArtist[];
};

export type BecauseYouListenTo = {
	seed_artist: string;
	seed_artist_mbid: string;
	listen_count: number;
	section: HomeSection;
	banner_url?: string | null;
	wide_thumb_url?: string | null;
	fanart_url?: string | null;
};

export type WeeklyExplorationTrack = {
	title: string;
	artist_name: string;
	album_name: string;
	recording_mbid: string | null;
	artist_mbid: string | null;
	release_group_mbid: string | null;
	cover_url: string | null;
	duration_ms: number | null;
};

export type WeeklyExplorationSection = {
	title: string;
	playlist_date: string;
	tracks: WeeklyExplorationTrack[];
	source_url: string;
};

export interface TopPickItem {
	album: HomeAlbum;
	match_pct: number;
	reasons: string[];
	seed_artist: string | null;
}

export interface TopPicksSection {
	title: string;
	items: TopPickItem[];
	source: string | null;
	// still trending-only while personalisation resolves during a ListenBrainz outage
	personalizing: boolean;
}

export type DiscoverResponse = {
	because_you_listen_to: BecauseYouListenTo[];
	discover_queue_enabled: boolean;
	fresh_releases: HomeSection | null;
	missing_essentials: HomeSection | null;
	rediscover: HomeSection | null;
	artists_you_might_like: HomeSection | null;
	popular_in_your_genres: HomeSection | null;
	genre_list: HomeSection | null;
	globally_trending: HomeSection | null;
	weekly_exploration: WeeklyExplorationSection | null;
	lastfm_weekly_artist_chart: HomeSection | null;
	lastfm_weekly_album_chart: HomeSection | null;
	lastfm_recent_scrobbles: HomeSection | null;
	daily_mixes: HomeSection[];
	radio_sections: HomeSection[];
	top_picks: TopPicksSection | null;
	listeners_like_you: HomeSection | null;
	anniversaries: HomeSection | null;
	new_from_followed: HomeSection | null;
	unexplored_genres: HomeSection | null;
	genre_artists: Record<string, string | null>;
	genre_artist_images: Record<string, string | null>;
	integration_status: Record<string, boolean>;
	service_prompts: ServicePrompt[];
	refreshing: boolean;
	service_status: Record<string, string> | null;
};

export type PlaylistProfile = {
	artist_mbids: string[];
	genre_distribution: Record<string, string[]>;
	track_count: number;
};

export type PlaylistSuggestionsResponse = {
	suggestions: HomeSection;
	playlist_id: string;
	profile: PlaylistProfile;
};

export type GenreLibrarySection = {
	artists: HomeArtist[];
	albums: HomeAlbum[];
	artist_count: number;
	album_count: number;
};

export type GenrePopularSection = {
	artists: HomeArtist[];
	albums: HomeAlbum[];
	has_more_artists: boolean;
	has_more_albums: boolean;
};

export type GenreDetailResponse = {
	genre: string;
	library: GenreLibrarySection | null;
	popular: GenrePopularSection | null;
	artists: HomeArtist[];
	total_count: number | null;
};

export type SimilarArtist = {
	musicbrainz_id: string;
	name: string;
	listen_count: number;
	in_library: boolean;
	image_url?: string | null;
};

export type SimilarArtistsResponse = {
	similar_artists: SimilarArtist[];
	source: string;
	configured: boolean;
};

export type TopSong = {
	recording_mbid?: string | null;
	release_group_mbid?: string | null;
	original_release_mbid?: string | null;
	title: string;
	artist_name: string;
	release_name?: string | null;
	listen_count: number;
	disc_number?: number | null;
	track_number?: number | null;
};

export type TopSongsResponse = {
	songs: TopSong[];
	source: string;
	configured: boolean;
};

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

export type TopAlbum = {
	release_group_mbid?: string | null;
	title: string;
	artist_name: string;
	year?: number | null;
	listen_count: number;
	in_library: boolean;
	requested?: boolean;
	cover_url?: string | null;
};

export type TopAlbumsResponse = {
	albums: TopAlbum[];
	source: string;
	configured: boolean;
};

export type DiscoveryAlbum = {
	musicbrainz_id: string;
	title: string;
	artist_name: string;
	artist_id?: string | null;
	year?: number | null;
	in_library: boolean;
	requested?: boolean;
	cover_url?: string | null;
};

export type SimilarAlbumsResponse = {
	albums: DiscoveryAlbum[];
	source: string;
	configured: boolean;
};

export type DiscoverQueueItemLight = {
	release_group_mbid: string;
	album_name: string;
	artist_name: string;
	artist_mbid: string;
	cover_url: string | null;
	recommendation_reason: string;
	is_wildcard: boolean;
	in_library: boolean;
};

export type DiscoverQueueEnrichment = {
	artist_mbid: string | null;
	release_date: string | null;
	country: string | null;
	tags: string[];
	youtube_url: string | null;
	youtube_search_url: string;
	youtube_search_available: boolean;
	artist_description: string | null;
	listen_count: number | null;
};

export type YouTubeSearchResponse = {
	video_id: string | null;
	embed_url: string | null;
	error: string | null;
	cached: boolean;
};

export type YouTubeQuotaStatus = {
	used: number;
	limit: number;
	remaining: number;
	date: string;
};

export type TrackCacheCheckItem = {
	artist: string;
	track: string;
	cached: boolean;
};

export type DiscoverQueueItemFull = DiscoverQueueItemLight & {
	enrichment?: DiscoverQueueEnrichment;
	// client-side marker set after the user requests the album from the deck
	requested?: boolean;
};

export type DiscoverQueueResponse = {
	items: DiscoverQueueItemFull[];
	queue_id: string;
};

export type MoreByArtistResponse = {
	albums: DiscoveryAlbum[];
	artist_name: string;
};

export type YouTubeLink = {
	album_id: string;
	video_id: string | null;
	album_name: string;
	artist_name: string;
	embed_url: string | null;
	cover_url: string | null;
	created_at: string;
	is_manual: boolean;
	track_count: number;
};

export type YouTubeLinkResponse = {
	link: YouTubeLink;
	quota: YouTubeQuotaStatus;
};

export type YouTubeTrackLink = {
	album_id: string;
	track_number: number;
	disc_number?: number | null;
	track_name: string;
	video_id: string;
	artist_name: string;
	embed_url: string;
	created_at: string;
	album_name?: string;
};

export type YouTubeTrackLinkResponse = {
	track_link: YouTubeTrackLink;
	quota: YouTubeQuotaStatus;
};

export type YouTubeTrackLinkBatchResponse = {
	track_links: YouTubeTrackLink[];
	failed: {
		track_number: number;
		disc_number?: number | null;
		track_name: string;
		reason: string;
	}[];
	quota: YouTubeQuotaStatus;
};

export type StatusMessage = {
	title?: string | null;
	messages: string[];
};

export type ActiveRequestItem = {
	musicbrainz_id: string;
	artist_name: string;
	album_title: string;
	artist_mbid?: string | null;
	year?: number | null;
	cover_url?: string | null;
	requested_at: string;
	status: string;
	progress?: number | null;
	eta?: string | null;
	size?: number | null;
	size_remaining?: number | null;
	download_status?: string | null;
	download_state?: string | null;
	status_messages?: StatusMessage[] | null;
	error_message?: string | null;
	library_queue_id?: number | null;
	quality?: string | null;
	protocol?: string | null;
	download_client?: string | null;
	user_id?: string | null;
	requested_by_name?: string | null;
};

export type RequestHistoryItem = {
	musicbrainz_id: string;
	artist_name: string;
	album_title: string;
	artist_mbid?: string | null;
	year?: number | null;
	cover_url?: string | null;
	requested_at: string;
	completed_at?: string | null;
	status: string;
	in_library: boolean;
	user_id?: string | null;
	requested_by_name?: string | null;
	reviewed_by_name?: string | null;
	reviewed_at?: string | null;
	download_task_id?: string | null;
	can_reimport?: boolean;
};

export type ActiveRequestsResponse = {
	items: ActiveRequestItem[];
	count: number;
};

export type RequestHistoryResponse = {
	items: RequestHistoryItem[];
	total: number;
	page: number;
	page_size: number;
	total_pages: number;
};

export type JellyfinTrackInfo = {
	jellyfin_id: string;
	title: string;
	track_number: number;
	disc_number?: number | null;
	duration_seconds: number;
	album_name: string;
	artist_name: string;
	album_id?: string;
	codec?: string | null;
	bitrate?: number | null;
	image_url?: string | null;
};

export type JellyfinAlbumMatch = {
	found: boolean;
	jellyfin_album_id?: string | null;
	tracks: JellyfinTrackInfo[];
};

export type JellyfinAlbumSummary = {
	jellyfin_id: string;
	name: string;
	artist_name: string;
	year?: number | null;
	track_count: number;
	image_url?: string | null;
	musicbrainz_id?: string | null;
	artist_musicbrainz_id?: string | null;
	play_count?: number;
};

export type JellyfinPaginatedResponse = {
	items: JellyfinAlbumSummary[];
	total: number;
	offset: number;
	limit: number;
};

export type JellyfinLibraryStats = {
	total_tracks: number;
	total_albums: number;
	total_artists: number;
};

export type JellyfinArtistSummary = {
	jellyfin_id: string;
	name: string;
	image_url?: string | null;
	album_count: number;
	musicbrainz_id?: string | null;
	play_count?: number;
};

export type JellyfinArtistPage = {
	items: JellyfinArtistSummary[];
	total: number;
	offset: number;
	limit: number;
};

export type JellyfinArtistIndexEntry = {
	name: string;
	artists: JellyfinArtistSummary[];
};

export type JellyfinArtistIndexResponse = {
	index: JellyfinArtistIndexEntry[];
};

export type JellyfinTrackPage = {
	items: JellyfinTrackInfo[];
	total: number;
	offset: number;
	limit: number;
};

export type NavidromeConnectionSettings = {
	navidrome_url: string;
	username: string;
	password: string;
	enabled: boolean;
};

export type NavidromeTrackInfo = {
	navidrome_id: string;
	title: string;
	track_number: number;
	disc_number?: number | null;
	duration_seconds: number;
	album_name: string;
	artist_name: string;
	codec?: string | null;
	bitrate?: number | null;
	image_url?: string | null;
};

export type NavidromeAlbumSummary = {
	navidrome_id: string;
	name: string;
	artist_name: string;
	year?: number | null;
	track_count: number;
	image_url?: string | null;
	musicbrainz_id?: string | null;
	artist_musicbrainz_id?: string | null;
};

export type NavidromeAlbumDetail = NavidromeAlbumSummary & {
	tracks: NavidromeTrackInfo[];
};

export type NavidromeAlbumMatch = {
	found: boolean;
	navidrome_album_id?: string | null;
	tracks: NavidromeTrackInfo[];
};

export type NavidromeArtistSummary = {
	navidrome_id: string;
	name: string;
	image_url?: string | null;
	album_count: number;
	musicbrainz_id?: string | null;
};

export type NavidromeSearchResponse = {
	albums: NavidromeAlbumSummary[];
	artists: NavidromeArtistSummary[];
	tracks: NavidromeTrackInfo[];
};

export type NavidromeArtistIndexEntry = {
	name: string;
	artists: NavidromeArtistSummary[];
};

export type NavidromeArtistIndexResponse = {
	index: NavidromeArtistIndexEntry[];
};

export type NavidromeArtistPage = {
	items: NavidromeArtistSummary[];
	total: number;
	offset: number;
	limit: number;
};

export type NavidromeTrackPage = {
	items: NavidromeTrackInfo[];
	total: number;
	offset: number;
	limit: number;
};

export type NavidromeGenreSongsResponse = {
	songs: NavidromeTrackInfo[];
	genre: string;
};

export type NavidromeLibraryStats = {
	total_tracks: number;
	total_albums: number;
	total_artists: number;
};

export type NavidromePaginatedResponse = {
	items: NavidromeAlbumSummary[];
	total: number;
};

export type PlexConnectionSettings = {
	plex_url: string;
	plex_token: string;
	enabled: boolean;
	login_enabled: boolean;
	music_library_ids: string[];
	scrobble_to_plex: boolean;
};

export type PlexTrackInfo = {
	plex_id: string;
	title: string;
	track_number: number;
	duration_seconds: number;
	disc_number: number;
	album_name: string;
	artist_name: string;
	codec?: string | null;
	bitrate?: number | null;
	audio_channels?: number | null;
	container?: string | null;
	part_key?: string | null;
	image_url?: string | null;
};

export type PlexAlbumSummary = {
	plex_id: string;
	name: string;
	artist_name: string;
	year?: number | null;
	track_count: number;
	image_url?: string | null;
	musicbrainz_id?: string | null;
	artist_musicbrainz_id?: string | null;
	last_viewed_at?: number;
};

export type PlexAlbumDetail = PlexAlbumSummary & {
	tracks: PlexTrackInfo[];
	genres: string[];
};

export type PlexAlbumMatch = {
	found: boolean;
	plex_album_id?: string | null;
	tracks: PlexTrackInfo[];
};

export type PlexArtistSummary = {
	plex_id: string;
	name: string;
	image_url?: string | null;
	musicbrainz_id?: string | null;
};

export type PlexSearchResponse = {
	albums: PlexAlbumSummary[];
	artists: PlexArtistSummary[];
	tracks: PlexTrackInfo[];
};

export type PlexLibraryStats = {
	total_tracks: number;
	total_albums: number;
	total_artists: number;
};

export type PlexPaginatedResponse = {
	items: PlexAlbumSummary[];
	total: number;
};

export type PlexArtistPage = {
	items: PlexArtistSummary[];
	total: number;
	offset: number;
	limit: number;
};

export type PlexArtistIndexEntry = {
	name: string;
	artists: PlexArtistSummary[];
};

export type PlexArtistIndexResponse = {
	index: PlexArtistIndexEntry[];
};

export type PlexTrackPage = {
	items: PlexTrackInfo[];
	total: number;
	offset: number;
	limit: number;
};

export type PlexLibrarySection = {
	key: string;
	title: string;
};

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

export type PlexHubResponse = {
	stats: PlexLibraryStats | null;
	recently_played: PlexAlbumSummary[];
	recently_added: PlexAlbumSummary[];
	all_albums_preview: PlexAlbumSummary[];
	playlists: SourcePlaylistSummary[];
	genres: string[];
};

export type PlexDiscoveryAlbum = {
	plex_id: string;
	name: string;
	artist_name: string;
	year?: number | null;
	image_url?: string | null;
};

export type PlexDiscoveryHub = {
	title: string;
	hub_type: string;
	albums: PlexDiscoveryAlbum[];
};

export type PlexDiscoveryResponse = {
	hubs: PlexDiscoveryHub[];
};

export type NavidromeHubResponse = {
	stats: NavidromeLibraryStats | null;
	recently_played: NavidromeAlbumSummary[];
	favorites: NavidromeAlbumSummary[];
	favorite_artists: NavidromeArtistSummary[];
	favorite_tracks: NavidromeTrackInfo[];
	all_albums_preview: NavidromeAlbumSummary[];
	playlists: SourcePlaylistSummary[];
	genres: string[];
};

export type JellyfinHubResponse = {
	stats: JellyfinLibraryStats | null;
	recently_played: JellyfinAlbumSummary[];
	recently_added: JellyfinAlbumSummary[];
	favorites: JellyfinAlbumSummary[];
	most_played_artists: JellyfinArtistSummary[];
	most_played_albums: JellyfinAlbumSummary[];
	all_albums_preview: JellyfinAlbumSummary[];
	playlists: SourcePlaylistSummary[];
	genres: string[];
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

export type LastFmConnectionSettingsResponse = {
	api_key: string;
	shared_secret: string;
	session_key: string;
	username: string;
	enabled: boolean;
};

export type SpotifySettings = {
	client_id: string;
	client_secret: string;
	enabled: boolean;
};

// mirrors backend api/v1/schemas/settings.py (FreeMusicSettings)
export type FreeMusicSettings = {
	enabled: boolean;
	preferred_format: 'flac' | 'mp3';
};

// mirrors backend api/v1/schemas/settings.py (GetItSettings)
export type GetItSettings = {
	store_region: string; // ISO 3166-1 alpha-2, feeds the iTunes storefront
	support_droppedneedle: boolean; // D19 affiliate toggle
};

// mirrors backend api/v1/schemas/get_it.py
export type PurchaseLink = {
	store: string;
	label: string;
	url: string;
	kind: 'digital' | 'physical' | 'free';
};

export type PurchaseOptionsResponse = {
	digital: PurchaseLink[];
	physical: PurchaseLink[];
	free: PurchaseLink[];
	bandcamp_search_url: string;
	disclosure: boolean;
};

export type ArtistPurchaseOptionsResponse = {
	links: PurchaseLink[];
	bandcamp_search_url: string;
	disclosure: boolean;
};

// mirrors backend api/v1/schemas/settings.py (EventsSettings)
export type EventsSettings = {
	enabled: boolean;
	ticketmaster_enabled: boolean;
	ticketmaster_api_key: string;
	skiddle_enabled: boolean;
	skiddle_api_key: string;
	poll_time: string; // server-local HH:MM, daily sweep slot
	sweep_scope: 'followed' | 'library';
};

export type WrappedSettingsResponse = {
	api_key: string;
};

export type SpotifyPlaylistItem = {
	id: string;
	name: string;
	description: string;
	track_count: number;
	cover_url: string | null;
	owner: string;
	imported_playlist_id: string | null;
};

export type SpotifyPlaylistListResponse = {
	playlists: SpotifyPlaylistItem[];
};

export type SpotifyImportResponse = {
	playlist_id: string;
};

export type ScrobbleSettings = {
	scrobble_to_lastfm: boolean;
	scrobble_to_listenbrainz: boolean;
};

export type NowPlayingSubmission = {
	track_name: string;
	artist_name: string;
	album_name: string;
	duration_ms: number;
	mbid?: string;
};

export type ScrobbleSubmission = {
	track_name: string;
	artist_name: string;
	album_name: string;
	timestamp: number;
	duration_ms: number;
	mbid?: string;
};

export type ServiceResult = {
	success: boolean;
	error?: string;
};

export type ScrobbleResponse = {
	accepted: boolean;
	services: Record<string, ServiceResult>;
};

export type LastFmTag = {
	name: string;
	url?: string | null;
};

export type LastFmSimilarArtistDetail = {
	name: string;
	mbid?: string | null;
	match: number;
	url?: string | null;
};

export type LastFmArtistEnrichment = {
	bio?: string | null;
	summary?: string | null;
	tags: LastFmTag[];
	listeners: number;
	playcount: number;
	similar_artists: LastFmSimilarArtistDetail[];
	url?: string | null;
};

export type LastFmAlbumEnrichment = {
	summary?: string | null;
	tags: LastFmTag[];
	listeners: number;
	playcount: number;
	url?: string | null;
};

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

export type SourceImportResult = {
	droppedneedle_playlist_id: string;
	tracks_imported: number;
	tracks_failed: number;
	already_imported: boolean;
};

export type PlexSessionInfo = {
	session_id: string;
	user_name: string;
	track_title: string;
	artist_name: string;
	album_name: string;
	cover_url: string;
	player_device: string;
	player_platform: string;
	player_state: string;
	is_direct_play: boolean;
	progress_ms: number;
	duration_ms: number;
	audio_codec: string;
	audio_channels: number;
	bitrate: number;
};

export type PlexSessionsResponse = {
	sessions: PlexSessionInfo[];
	available: boolean;
};

export type NavidromeNowPlayingEntry = {
	user_name: string;
	minutes_ago: number;
	player_name: string;
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id: string;
	cover_art_id: string;
	duration_seconds: number;
	estimated_position_seconds?: number;
};

export type NavidromeNowPlayingResponse = {
	entries: NavidromeNowPlayingEntry[];
};

export type JellyfinSessionInfo = {
	session_id: string;
	user_name: string;
	device_name: string;
	client_name: string;
	track_name: string;
	artist_name: string;
	album_name: string;
	album_id: string;
	cover_url: string;
	position_seconds: number;
	duration_seconds: number;
	is_paused: boolean;
	play_method: string;
	audio_codec: string;
	bitrate: number;
};

export type JellyfinSessionsResponse = {
	sessions: JellyfinSessionInfo[];
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

export type NavidromeArtistInfo = {
	navidrome_id: string;
	name: string;
	biography: string;
	image_url: string;
	similar_artists: NavidromeArtistSummary[];
};

export type PlexHistoryEntry = {
	rating_key: string;
	track_title: string;
	artist_name: string;
	album_name: string;
	cover_url: string;
	viewed_at: string;
	device_name: string;
};

export type PlexHistoryResponse = {
	entries: PlexHistoryEntry[];
	total: number;
	limit: number;
	offset: number;
	available: boolean;
};

export type PlexAnalyticsItem = {
	name: string;
	subtitle: string;
	play_count: number;
	cover_url: string | null;
};

export type PlexAnalyticsResponse = {
	top_artists: PlexAnalyticsItem[];
	top_albums: PlexAnalyticsItem[];
	top_tracks: PlexAnalyticsItem[];
	total_listens: number;
	listens_last_7_days: number;
	listens_last_30_days: number;
	total_hours: number;
	is_complete: boolean;
	entries_analyzed: number;
};

export type LyricLine = {
	text: string;
	start_seconds: number | null;
};

export type NavidromeLyricsResponse = {
	text: string;
	is_synced: boolean;
	lines: LyricLine[];
};

export type JellyfinLyricsLine = {
	text: string;
	start_seconds: number | null;
};

export type JellyfinLyricsResponse = {
	lines: JellyfinLyricsLine[];
	is_synced: boolean;
	lyrics_text: string;
};

export type JellyfinFavoritesExpanded = {
	albums: JellyfinAlbumSummary[];
	artists: JellyfinArtistSummary[];
};

export type JellyfinFilterFacets = {
	years: number[];
	tags: string[];
	studios: string[];
};

export type AlbumSort = 'recent' | 'title' | 'artist';

export type TrackSort = 'recent' | 'title' | 'artist' | 'album';

export type NativeTrackListItem = {
	track_file_id: string;
	title: string;
	album_name: string;
	artist_name: string;
	album_mbid?: string | null;
	cover_url?: string | null;
	format: string;
	track_number: number;
	disc_number: number;
	duration_seconds?: number | null;
};

export type NativeTrackPage = {
	items: NativeTrackListItem[];
	total: number;
	offset: number;
	limit: number;
};

export interface LibraryAlbumSummary {
	release_group_mbid: string;
	album_title: string;
	album_artist_name: string | null;
	track_count: number;
	total_size_bytes: number;
	quality_format: string | null;
	year: number | null;
	is_compilation: boolean;
	cover_url: string | null;
	last_imported_at: number | null;
}

export interface NativeAlbumsResponse {
	items: LibraryAlbumSummary[];
	total: number;
}

export type ArtistSort = 'name' | 'album_count' | 'date_added';

export interface LibraryArtistSummary {
	artist_name: string;
	artist_mbid: string | null;
	album_count: number;
	track_count: number;
	date_added: number | null;
}

export interface NativeArtistsResponse {
	items: LibraryArtistSummary[];
	total: number;
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

export type LibraryFileMeta = LibraryTrack;

export interface LibraryAlbumStatus {
	in_library: boolean;
	track_count: number;
	tracks: LibraryTrack[];
	// Coverage vs the release's MB tracklist (P5): expected_tracks === 0 means the
	// tracklist was unavailable and the UI falls back to the presence-only reading.
	expected_tracks: number;
	covered_tracks: number;
	matched_file_ids: string[];
	// held files that match NONE of the album's expected tracks ("doesn't match")
	orphans: LibraryTrack[];
}

export interface AlbumEditionItem {
	release_mbid: string;
	track_count: number;
	title: string | null;
	disambiguation: string | null;
	date: string | null;
	country: string | null;
	packaging: string | null;
	status: string | null;
	is_owned: boolean;
	is_pinned: boolean;
}

export interface AlbumEditionsResponse {
	items: AlbumEditionItem[];
	pinned_release_mbid: string | null;
	owned_release_mbid: string | null;
}

export interface EditionAcquireResponse {
	release_mbid: string;
	total_tracks: number;
	requested: number;
	upgrades: number;
	skipped: number;
}

export interface CutoffUnmetItem {
	release_group_mbid: string;
	current_tier: string;
	track_count: number;
	artist_name: string | null;
	artist_mbid: string | null;
	album_title: string | null;
	year: number | null;
}

export interface CutoffUnmetResponse {
	items: CutoffUnmetItem[];
	cutoff: string;
	upgrade_allowed: boolean;
}

export interface UpgradeRequestResponse {
	status: 'queued' | 'satisfied';
	task_id: string | null;
}

export interface LibraryStats {
	total_albums: number;
	total_artists: number;
	total_tracks: number;
	total_size_bytes: number;
	format_breakdown: Record<string, number>;
	unmatched_count: number;
	last_scan_at: number | null;
	recently_added: LibraryAlbumSummary[];
}

export interface ManualReviewEntry {
	id: number;
	file_path: string;
	extracted_title: string | null;
	extracted_artist: string | null;
	extracted_album: string | null;
	extracted_year: number | null;
	track_number: number | null;
	disc_number: number | null;
	file_format: string | null;
	duration: number | null;
	file_size: number | null;
	fingerprint: string | null;
	fingerprint_score: number | null;
	candidate_mbids: string[];
	source: string;
	created_at: number | null;
}

export interface LibraryUnmatchedResponse {
	items: ManualReviewEntry[];
	total: number;
}

export interface UnmatchedBatchItem {
	review_id: number;
	recording_mbid: string | null;
}

export interface UnmatchedBatchResolveRequest {
	release_group_mbid: string;
	items: UnmatchedBatchItem[];
}

export interface UnmatchedBatchResolveResponse {
	resolved: number;
	failed: { review_id: number; error: string }[];
}

export type ScanStatus = 'idle' | 'scanning' | 'complete' | 'cancelled' | 'failed';

export interface LibraryScanStatus {
	status: ScanStatus;
	total_files: number;
	processed_files: number;
	matched_files: number;
	failed_files: number;
	started_at: number | null;
	updated_at: number | null;
}

export interface LibrarySettings {
	library_paths: string[];
	staging_path: string;
	naming_template: string;
	acoustid_api_key: string;
}

export type ScanFrequency =
	| 'manual'
	| '5min'
	| '10min'
	| '30min'
	| '1hr'
	| '6hr'
	| '12hr'
	| '24hr'
	| '3d'
	| '7d'
	| 'daily';

export interface LibraryScanSchedule {
	scan_frequency: ScanFrequency;
	/** HH:MM (server-local) used when scan_frequency is 'daily'. */
	daily_scan_time: string;
	last_scan: number | null;
	last_scan_success: boolean;
	/** Server timezone label for the daily-time caption; present on reads only. */
	server_timezone?: string;
}

export interface TrackTagUpdate {
	title: string;
	artist: string;
	album: string;
	track_number: number;
	album_artist: string | null;
	disc_number: number;
	year: number | null;
	genre: string | null;
	musicbrainz_release_group_id: string | null;
	musicbrainz_release_id: string | null;
	musicbrainz_recording_id: string | null;
	musicbrainz_artist_id: string | null;
	musicbrainz_album_artist_id: string | null;
}

export type UnmatchedResolution = 'accept' | 'reject' | 'manual_id';

export interface LibraryActionResponse {
	status: string;
	message: string;
}

// mirrors backend api/v1/schemas/common.py StatusMessageResponse
export interface StatusMessageResponse {
	status: string;
	message: string;
}

export interface DownloadClientConfig {
	enabled: boolean;
	client_type: string;
	url: string;
	api_key: string;
	verify_downloads: boolean;
	min_bitrate_kbps: number;
	quality_min: string;
	quality_max: string;
	flac_mp3_only: boolean;
	downloads_subpath: string;
	preflight_score_auto_accept: number;
	preflight_score_manual_min: number;
	download_stall_timeout_minutes: number;
	download_queued_timeout_minutes: number;
	max_failover_attempts: number;
	max_concurrent_downloads: number;
}

export interface DownloadsMountStatus {
	ok: boolean;
	reason: string;
	path: string;
}

export interface DownloadClientHealth {
	status: 'ok' | 'error';
	version?: string | null;
	message?: string | null;
}

export interface DownloadClientStatus {
	configured: boolean;
	client: DownloadClientHealth;
	mount: DownloadsMountStatus;
	mount_advisory?: string | null;
	slskd_downloads_dir?: string | null;
}

export interface TestConnectionResult {
	valid: boolean;
	version?: string | null;
	message: string;
}

export interface IndexerSettings {
	id: string;
	type: string;
	name: string;
	url: string;
	api_key: string;
	categories: number[];
	enabled: boolean;
	priority: number;
}

export interface IndexerTestResult {
	valid: boolean;
	version?: string | null;
	message: string;
	supports_audio_search: boolean;
	category_count: number;
	suggested_url?: string | null;
}

export interface IndexerSavedResponse {
	id: string;
}

export interface OperationResult {
	success: boolean;
	message?: string | null;
}

// Acquisition plugins (hand-mirrors backend api/v1/schemas/plugins.py)
export interface PluginInfo {
	id: string;
	name: string;
	version: string;
	builtin: boolean;
	enabled: boolean;
	loaded: boolean;
	error?: string | null;
	source: string;
	api_version?: number | null;
}

export interface PluginListResponse {
	plugins: PluginInfo[];
	api_version: number;
}

export type PluginSettingsValue = string | number | boolean | null;

export interface PluginSelectOption {
	value: string | number;
	label: string;
}

export interface PluginSettingsField {
	key: string;
	type: 'str' | 'int' | 'bool' | 'select' | 'secret';
	label: string;
	help: string;
	default: PluginSettingsValue;
	required: boolean;
	options: PluginSelectOption[];
}

export interface PluginSettingsResponse {
	id: string;
	schema: PluginSettingsField[];
	values: Record<string, PluginSettingsValue>;
}

export interface PluginToggleResponse {
	id: string;
	enabled: boolean;
}

export interface PluginTestResult {
	valid: boolean;
	message: string;
	version?: string | null;
}

export interface SabnzbdConnectionSettings {
	enabled: boolean;
	client_type: string;
	url: string;
	api_key: string;
	category: string;
	priority: number;
	post_processing: number;
	downloads_mount: string;
}

export interface SabnzbdTestResult {
	valid: boolean;
	version?: string | null;
	message: string;
	categories: string[];
	complete_dir?: string | null;
}

export interface SourcePriority {
	order: string[];
}

export interface DownloadPolicySettings {
	quality_min: string;
	quality_max: string;
	flac_mp3_only: boolean;
	verify_downloads: boolean;
	preflight_score_auto_accept: number;
	preflight_score_manual_min: number;
	download_stall_timeout_minutes: number;
	download_queued_timeout_minutes: number;
	max_failover_attempts: number;
	max_concurrent_downloads: number;
	auto_retry_enabled: boolean;
	auto_retry_max_attempts: number;
	auto_retry_base_interval_minutes: number;
	usenet_min_release_age_minutes: number;
	quality_cutoff: string;
	upgrade_allowed: boolean;
	max_library_size_gb: number;
	default_request_quota_count: number;
	default_request_quota_days: number;
	default_storage_quota_gb: number;
	background_upgrade_scan_enabled: boolean;
	background_upgrade_scan_interval_hours: number;
	background_upgrade_max_per_run: number;
}

// Hand-mirrors backend WantedWatcherSettings (api/v1/schemas/settings.py).
export interface WantedWatcherSettings {
	enabled: boolean;
	auto_download_on_find: boolean;
	watch_partial_albums: boolean;
	max_checks_per_sweep: number;
	dormant_after_days: number;
}

export interface DownloadSearchResultFile {
	username: string;
	filename: string;
	parent_directory: string;
	size: number;
	extension: string;
	bitrate?: number | null;
	bit_depth?: number | null;
	sample_rate?: number | null;
	duration?: number | null;
	has_free_slot: boolean;
	upload_speed: number;
}

export type CandidateTier = 'auto' | 'manual' | 'rejected';

export interface UsenetRelease {
	indexer_id: string;
	indexer_name: string;
	guid: string;
	title: string;
	nzb_url: string;
	size_bytes: number;
	category_ids: number[];
	grabs?: number | null;
	files?: number | null;
	usenet_date?: number | null;
}

export interface ScoredCandidate {
	// "soulseek" | "usenet" - selects the review-card variant (D16). Optional for
	// backward-compat with older cached candidate blobs (default soulseek).
	source?: string;
	username: string;
	parent_directory: string;
	files: DownloadSearchResultFile[];
	usenet_release?: UsenetRelease | null;
	coherence: number;
	file_confidence: number;
	final_score: number;
	tier: CandidateTier;
}

export interface SearchAlbumResponse {
	status: string;
	job_id: string | null;
}

export interface SearchJobView {
	job_id: string;
	status: string;
	artist_name: string;
	album_title: string;
	candidate_count: number;
	top_score?: number | null;
	candidates: ScoredCandidate[];
}

export interface PickResponse {
	task_id: string;
}

// Hand-mirrors backend DismissReviewResponse (api/v1/schemas/download.py):
// "None of these - keep watching" put the album on the wanted watchlist.
export interface DismissReviewResponse {
	success: boolean;
	state: string;
}

export type DownloadStatus =
	| 'queued'
	| 'downloading'
	| 'processing'
	| 'completed'
	| 'partial'
	| 'failed'
	| 'cancelled';

// mirrors backend DownloadTaskResponse (api/v1/schemas/download.py)
export interface DownloadTask {
	id: string;
	user_id: string;
	download_type: string;
	// "soulseek" | "usenet" - drives the source badge + the "via album NZB" label.
	// Optional for backward-compat with cached/older responses.
	source?: string;
	release_group_mbid: string;
	recording_mbid: string | null;
	// Backfilled from the release group at request time; older tasks predating the
	// backfill (or one MusicBrainz couldn't resolve) have no artist link.
	artist_mbid?: string | null;
	artist_name: string;
	album_title: string;
	track_title: string | null;
	year: number | null;
	status: DownloadStatus;
	progress_percent: number;
	total_size_bytes: number | null;
	downloaded_bytes: number;
	files_total: number;
	files_completed: number;
	files_failed: number;
	source_username: string | null;
	search_job_id: string | null;
	candidate_index: number | null;
	preflight_score: number | null;
	final_path: string | null;
	error_message: string | null;
	retry_count: number;
	created_at: number;
	updated_at: number;
	completed_at: number | null;
	next_retry_at: number | null;
	retry_max: number;
	// The full auto-retry backoff schedule in minutes for this task's max attempts, e.g.
	// [15, 30, 60, 120, 240, 480]. Empty when auto-retry is off. Drives the Wanted ladder.
	retry_ladder_minutes: number[];
}

export interface DownloadListResponse {
	items: DownloadTask[];
	page: number;
	page_size: number;
}

// A downloaded track that matched by duration but failed the AcoustID recording-identity
// check (usually wrong MusicBrainz metadata). Held for a human "import anyway" / "discard"
// decision instead of being dropped. `evidence_*` is what AcoustID heard.
export interface HeldImport {
	id: number;
	release_group_mbid: string | null;
	recording_mbid: string | null;
	track_number: number | null;
	disc_number: number | null;
	track_title: string | null;
	artist_name: string | null;
	album_title: string | null;
	year: number | null;
	original_filename: string | null;
	file_format: string | null;
	duration_seconds: number | null;
	reason: string;
	source: string;
	source_task_id: string | null;
	created_at: number;
	evidence_title: string | null;
	evidence_artist: string | null;
	evidence_score: number | null;
}

export interface HeldListResponse {
	items: HeldImport[];
}

// SSE payload on the `download:{task_id}` channel `progress` event
export interface DownloadProgress {
	bytes_downloaded: number;
	bytes_total: number;
	files_completed: number;
	files_total: number;
	progress_percent: number;
}

// mirrors backend RequestAcceptedResponse (api/v1/schemas/request.py)
// status is one of: "pending" | "awaiting_approval" | "downloading"
export interface RequestAccepted {
	success: boolean;
	message: string;
	musicbrainz_id: string;
	status: string;
}

export interface TrackRequestResponse {
	status: string;
	task_id?: string | null;
}

export interface CancelDownloadResponse {
	success: boolean;
	status?: string;
}

export interface RetryDownloadResponse {
	success: boolean;
	task_id: string;
}

export interface ReimportDownloadResponse {
	success: boolean;
	status: string;
	files_imported: number;
	files_failed: number;
	error_message?: string | null;
}

export interface QuarantineEntry {
	id: number;
	client_id: string;
	username: string;
	filename: string;
	reason: string;
	quarantined_at: number;
	release_group_mbid?: string | null;
}

export interface QuarantineListResponse {
	items: QuarantineEntry[];
	page: number;
}

// Connect Apps (inbound OpenSubsonic + Jellyfin compatibility)
export interface ConnectAppsSettings {
	subsonic_enabled: boolean;
	jellyfin_enabled: boolean;
	transcoding_enabled: boolean;
	transcode_default_format: 'mp3' | 'opus';
	transcode_max_bitrate_kbps: number;
	advertise_server_name: string;
	advertise_server_version: string;
	discover_mode: 'local-only' | 'lazy-mb' | 'use-scrobble-targets';
}

export interface AppPasswordView {
	id: string;
	name: string;
	created_at: string;
	last_used_at: string | null;
	last_client: string | null;
}

export interface AppPasswordListResponse {
	items: AppPasswordView[];
	cap: number;
	active_count: number;
}

export interface AppPasswordCreateResponse {
	secret: string;
	app_password: AppPasswordView;
}

// Admin oversight roster: every user's active app-passwords (never a secret).
export interface AdminAppPasswordView {
	id: string;
	user_id: string;
	owner_username: string;
	owner_display_name: string;
	name: string;
	created_at: string;
	last_used_at: string | null;
	last_client: string | null;
}

export interface AdminAppPasswordListResponse {
	items: AdminAppPasswordView[];
	active_count: number;
}

export interface SectionPrefItem {
	key: string;
	title: string;
	description: string;
	zone: string;
	enabled: boolean;
	available: boolean;
	requires: string | null;
}

export interface SectionPrefsResponse {
	pages: Record<string, SectionPrefItem[]>;
}

export interface SectionPrefsUpdate {
	page: 'home' | 'discover' | 'sidebar';
	sections: { key: string; enabled: boolean }[];
}

export interface PreviewTrackItem {
	title: string;
	artist_name: string;
	preview_url: string;
	duration_s: number | null;
	position: number | null;
}

export interface TrackPreviewResponse {
	preview_url: string | null;
	title: string | null;
	duration_s: number | null;
	provider: string | null;
}

export interface AlbumPreviewResponse {
	tracks: PreviewTrackItem[];
	provider: string | null;
}

export interface RadioSeedItem {
	artist_mbid: string;
	artist_name?: string;
	album_mbid?: string | null;
}

export interface RadioPlanRequest {
	seed_type: 'artist' | 'album' | 'genre' | 'items';
	seed_id?: string | null;
	items?: RadioSeedItem[];
	mode?: 'library' | 'hybrid';
	count?: number;
	exclude_recording_mbids?: string[];
	fast?: boolean;
}

export interface RadioPlanTrack {
	track_name: string;
	artist_name: string;
	artist_mbid: string;
	recording_mbid: string | null;
	album_mbid: string | null;
	album_name: string | null;
	in_library: boolean;
	local_file_id: string | null;
	file_format: string | null;
	duration_s: number | null;
}

export interface RadioPlanResponse {
	title: string;
	tracks: RadioPlanTrack[];
}

export interface DiscoveryBatchItemIn {
	release_group_mbid: string;
	artist_mbid: string;
	album_name: string;
	artist_name: string;
}

export interface DiscoveryBatchCreate {
	name: string;
	source_section: string;
	items: DiscoveryBatchItemIn[];
}

export interface DiscoveryBatchItemStatus extends DiscoveryBatchItemIn {
	outcome: 'requested' | 'skipped_in_library' | 'skipped_duplicate';
	request_status: string | null;
	in_library: boolean;
}

export interface DiscoveryBatchSummary {
	id: string;
	name: string;
	source_section: string;
	created_at: string;
	item_count: number;
	imported_count: number;
	pending_count: number;
}

export interface DiscoveryBatchDetail extends DiscoveryBatchSummary {
	items: DiscoveryBatchItemStatus[];
}

export interface DiscoveryBatchListResponse {
	batches: DiscoveryBatchSummary[];
}

export interface DiscoveryBatchRemoveResult {
	removed_albums: number;
	cancelled_requests: number;
	kept: number;
}
