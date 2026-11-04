import type { components } from '$lib/api/v3/openapi';

// The track and link shapes the player reads.
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
	recording_mbid?: string | null;
};

export type TrackView = components['schemas']['TrackView'];

export type PlaybackState =
	'idle' | 'loading' | 'playing' | 'paused' | 'ended' | 'buffering' | 'error';

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
	format?: string;
	playlistTrackId?: string;
	/** 30s preview stream (radio preview tier): never scrobbled/reported, fades out. */
	isPreview?: boolean;
	/** MusicBrainz recording the playing source gave for this file. */
	recordingMbid?: string;
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
	format?: string;
	availableSources?: SourceType[];
	sourceIds?: Partial<Record<SourceType, string>>;
	/** MusicBrainz recording each source gave for its file, kept per source. */
	recordingIds?: Partial<Record<SourceType, string>>;
	duration?: number;
	/** Plex ratingKey used for scrobble and now-playing calls. Streaming uses part_key in trackSourceId. */
	plexRatingKey?: string;
	queueOrigin?: QueueOrigin;
	/** Stable playlist-level track identifier that survives source changes. */
	playlistTrackId?: string;
	/** 30s preview stream (radio preview tier): never scrobbled/reported, fades out. */
	isPreview?: boolean;
}
