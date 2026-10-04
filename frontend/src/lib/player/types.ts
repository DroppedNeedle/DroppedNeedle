// Transitional v1 catalog shapes, copied verbatim from the deleted $lib/types
// (v3 HEAD). Producers (album/library routes) still emit these; the catalog
// slices re-home them when those surfaces migrate to v3 views. Player files
// import from here, never from $lib/types.
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

export type JellyfinAlbumMatch = {
	found: boolean;
	jellyfin_album_id?: string | null;
	tracks: JellyfinTrackInfo[];
};

export type LocalAlbumMatch = {
	found: boolean;
	musicbrainz_id?: string | null;
	tracks: LocalTrackInfo[];
	total_size_bytes: number;
	primary_format?: string | null;
	download_allowed?: boolean;
};

export type NavidromeAlbumMatch = {
	found: boolean;
	navidrome_album_id?: string | null;
	tracks: NavidromeTrackInfo[];
};

export type PlexAlbumMatch = {
	found: boolean;
	plex_album_id?: string | null;
	tracks: PlexTrackInfo[];
};

export type PlaybackState =
	| 'idle'
	| 'loading'
	| 'playing'
	| 'paused'
	| 'ended'
	| 'buffering'
	| 'error';

export type SourceType = 'youtube' | 'local' | 'jellyfin' | 'navidrome' | 'plex';

export type QueueOrigin = 'context' | 'manual';

export interface PlaybackSource {
	readonly type: SourceType;

	load(info: {
		trackSourceId?: string;
		url?: string;
		token?: string;
		format?: string;
		duration?: number;
	}): Promise<void>;
	play(): void;
	pause(): void;
	seekTo(seconds: number): void;
	setVolume(level: number): void;
	getCurrentTime(): number;
	getDuration(): number;
	isSeekable?(): boolean;
	destroy(): void;

	onStateChange(callback: (state: PlaybackState) => void): void;
	onReady(callback: () => void): void;
	onError(callback: (error: { code: string; message: string }) => void): void;
	onProgress(callback: (currentTime: number, duration: number) => void): void;
}

export interface NowPlaying {
	albumId: string;
	albumName: string;
	artistName: string;
	coverUrl: string | null;
	/** Raw remote cover URL (pre-proxy) so large displays reuse the browser-cached carousel image instead of cold-fetching covers. */
	coverRemoteUrl?: string | null;
	sourceType: SourceType;
	discNumber?: number;
	trackSourceId?: string;
	embedUrl?: string;
	trackName?: string;
	artistId?: string;
	streamUrl?: string;
	format?: string;
	playlistTrackId?: string;
	/** 30s preview stream (radio preview tier): never scrobbled/reported, fades out. */
	isPreview?: boolean;
}

export type PlaybackMeta = {
	albumId: string;
	albumName: string;
	artistName: string;
	coverUrl: string | null;
	artistId?: string;
};

export interface QueueItem {
	/** Source-specific item identifier (Jellyfin item ID, local file ID, or YouTube video ID). */
	trackSourceId: string;
	trackName: string;
	artistName: string;
	trackNumber: number;
	discNumber?: number;
	albumId: string;
	albumName: string;
	coverUrl: string | null;
	/** Raw remote cover URL (pre-proxy) so large displays reuse the browser-cached carousel image instead of cold-fetching covers. */
	coverRemoteUrl?: string | null;
	sourceType: SourceType;
	artistId?: string;
	streamUrl?: string;
	format?: string;
	availableSources?: SourceType[];
	sourceIds?: Partial<Record<SourceType, string>>;
	duration?: number;
	playSessionId?: string;
	/** Plex ratingKey used for scrobble and now-playing calls. Streaming uses part_key in trackSourceId. */
	plexRatingKey?: string;
	queueOrigin?: QueueOrigin;
	/** Stable playlist-level track identifier that survives source changes. */
	playlistTrackId?: string;
	/** 30s preview stream (radio preview tier): never scrobbled/reported, fades out. */
	isPreview?: boolean;
}
