import type { components } from '$lib/api/v3/openapi';

// mirrors backend api/v1/schemas/artist.py (FollowStatusResponse) and the
// following hub responses
export type AutoDownloadState = 'none' | 'pending' | 'approved' | 'rejected' | 'revoked';

export interface FollowStatus {
	followed: boolean;
	auto_download: boolean;
	auto_download_state: AutoDownloadState;
}

export interface FollowedArtist {
	mbid: string;
	name: string;
	image_url?: string | null;
	auto_download: boolean;
	auto_download_state: AutoDownloadState;
	followed_at: number;
}

export interface NewRelease {
	release_group_mbid: string;
	title: string;
	artist_name: string;
	artist_mbid: string;
	primary_type?: string | null;
	first_release_date?: string | null;
	in_library?: boolean; // meaningful on the recent-releases log view
}

export interface NewReleasesResponse {
	items: NewRelease[];
	total: number;
}

// mirrors backend api/v1/schemas/following.py (UnseenCountResponse)
export interface UnseenCountResponse {
	count: number;
}

// Approval rows come from the generated v3 contract (the follow rows above
// stay hand-mirrored for the follows migration). v3 drops the batch `source`
// tag and always resolves `user_name`.
export type AutoDownloadApproval = components['schemas']['AutoDownloadApprovalItem'];
export type AutoDownloadApprovalsResponse =
	components['schemas']['AutoDownloadApprovalListResponse'];
export type ApprovalBatch = components['schemas']['ApprovalBatchItem'];
export type ApprovalBatchListResponse = components['schemas']['ApprovalBatchListResponse'];
export type ApprovalActionResponse = components['schemas']['ActionResponse'];

// mirrors backend api/v1/schemas/following.py (ConcertResponse etc.)
export type ConcertStatus = 'scheduled' | 'cancelled' | 'rescheduled';

export interface Concert {
	artist_mbid: string;
	artist_name: string;
	event_name: string;
	local_date: string;
	status: ConcertStatus;
	source: 'ticketmaster' | 'skiddle';
	source_event_id: string;
	matched_city: string;
	venue_name?: string | null;
	city?: string | null;
	region?: string | null;
	country_code?: string | null;
	starts_at?: string | null;
	ticket_url?: string | null;
	distance_km?: number | null;
}

export interface ConcertsResponse {
	configured: boolean;
	items: Concert[];
	total: number;
}

export interface EventCity {
	city_name: string;
	latitude: number;
	longitude: number;
	radius_km: number;
	country_code?: string | null;
}

export interface EventCitiesResponse {
	items: EventCity[];
}

export interface CitySearchResult {
	name: string;
	latitude: number;
	longitude: number;
	country_code?: string | null;
	country?: string | null;
	region?: string | null;
}

export interface CitySearchResponse {
	items: CitySearchResult[];
}
