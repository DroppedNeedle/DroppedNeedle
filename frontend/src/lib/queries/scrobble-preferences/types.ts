import type { MusicSource } from '$lib/stores/musicSource';

// how a user's listening appears to others in the now-playing feed
export type NowPlayingVisibility = 'full' | 'track_hidden' | 'offline';

// mirrors the backend /api/v1/me/scrobble-preferences DTOs
export interface ScrobblePreferences {
	scrobble_to_lastfm: boolean;
	scrobble_to_listenbrainz: boolean;
	primary_music_source: string;
	now_playing_visibility: string;
	auto_request_personal_mix: boolean;
}

export interface ScrobblePreferencesUpdate {
	scrobble_to_lastfm?: boolean;
	scrobble_to_listenbrainz?: boolean;
	primary_music_source?: MusicSource;
	now_playing_visibility?: NowPlayingVisibility;
	auto_request_personal_mix?: boolean;
}

export interface PersonalMixRefreshResponse {
	playlist_id: string | null;
	track_count: number;
	requested_albums: number;
	skipped: boolean;
	reason: string;
}
