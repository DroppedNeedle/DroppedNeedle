// Hand-mirrors backend/api/v1/schemas/requests_page.py (WantedWatchItem,
// WantedWatchesResponse, WantedActionResponse) - update both sides together.

export type WantedKind = 'missing' | 'partial';
export type WantedState = 'watching' | 'dormant' | 'stopped' | 'fulfilled';
export type WantedOutcome =
	| 'no_results'
	| 'seen_only'
	| 'new_manual'
	| 'auto_dispatched'
	| 'satisfied'
	| 'error';

export interface WantedWatchItem {
	release_group_mbid: string;
	artist_name: string;
	album_title: string;
	kind: WantedKind;
	state: WantedState;
	check_count: number;
	next_check_at: number; // epoch seconds
	new_candidate_count: number;
	created_at: number; // epoch seconds
	artist_mbid: string | null;
	year: number | null;
	cover_url: string | null;
	first_release_date: string | null;
	last_checked_at: number | null; // epoch seconds
	last_outcome: WantedOutcome | null;
	user_id: string | null;
	user_name: string | null; // resolved for admin callers only (the "watched by" chip)
}

export interface WantedRetryingItem {
	release_group_mbid: string;
	artist_name: string;
	album_title: string;
	retry_count: number; // retries already spent; the upcoming attempt is retry_count + 1
	max_attempts: number;
	next_retry_at: number; // epoch seconds
	artist_mbid: string | null;
	year: number | null;
	cover_url: string | null;
	user_id: string | null;
	user_name: string | null;
}

export interface WantedWatchesResponse {
	items: WantedWatchItem[];
	count: number;
	// still in the auto-retry ladder; read-only rows that graduate into items
	retrying: WantedRetryingItem[];
}

export interface WantedActionResponse {
	success: boolean;
	state: WantedState;
}
