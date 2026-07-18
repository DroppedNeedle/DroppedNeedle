import type { MusicSource } from '$lib/stores/musicSource';

// how a user's listening appears to others in the now-playing feed
export type NowPlayingVisibility = 'full' | 'track_hidden' | 'offline';

// standing-grant state for Weekly Mix auto-request (admins read 'approved' by role)
export type AutoRequestState = 'none' | 'pending' | 'approved' | 'rejected' | 'revoked';

// mirrors the backend /api/v1/me/scrobble-preferences DTOs
export interface ScrobblePreferences {
	scrobble_to_lastfm: boolean;
	scrobble_to_listenbrainz: boolean;
	navidrome_handles_external_scrobbles: boolean;
	primary_music_source: string;
	now_playing_visibility: string;
	auto_request_personal_mix: boolean;
	auto_request_state: AutoRequestState;
}

export interface ScrobblePreferencesUpdate {
	scrobble_to_lastfm?: boolean;
	scrobble_to_listenbrainz?: boolean;
	navidrome_handles_external_scrobbles?: boolean;
	primary_music_source?: MusicSource;
	now_playing_visibility?: NowPlayingVisibility;
	auto_request_personal_mix?: boolean;
}

// the build runs in the background; the outcome arrives on the per-user SSE
// stream as a personal_mix_refreshed event
export interface PersonalMixRefreshResponse {
	status: 'started' | 'already_running';
}

export interface PersonalMixApprovalItem {
	user_id: string;
	requested_at: number; // epoch seconds
	user_name: string | null;
}

export interface PersonalMixApprovalsResponse {
	items: PersonalMixApprovalItem[];
	count: number;
}

export interface ApprovalActionResponse {
	success: boolean;
	message: string;
}
