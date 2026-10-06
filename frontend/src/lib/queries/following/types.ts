import type { components } from '$lib/api/v3/openapi';

// Page shapes for the following hub. The v3 wire types are adapted into
// these in FollowAdapters.ts (state strings narrowed, `artist_mbid` read as
// `mbid`).
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
	in_library?: boolean;
}

export interface NewReleasesResponse {
	items: NewRelease[];
	total: number;
}

export type UnseenCountResponse = components['schemas']['UnseenCountResponse'];

// Approval rows come from the generated v3 contract (the follow rows above
// stay hand-mirrored for the follows migration). v3 drops the batch `source`
// tag and always resolves `user_name`.
export type AutoDownloadApproval = components['schemas']['AutoDownloadApprovalItem'];
export type AutoDownloadApprovalsResponse =
	components['schemas']['AutoDownloadApprovalListResponse'];
export type ApprovalBatch = components['schemas']['ApprovalBatchItem'];
export type ApprovalBatchListResponse = components['schemas']['ApprovalBatchListResponse'];
export type ApprovalActionResponse = components['schemas']['ActionResponse'];

// Concert shapes from the generated v3 contract.
export type ConcertStatus = components['schemas']['ConcertStatus'];
export type Concert = components['schemas']['Concert'];
export type ConcertsResponse = components['schemas']['ConcertsResponse'];
export type EventCity = components['schemas']['EventCity'];
export type EventCitiesResponse = components['schemas']['EventCitiesResponse'];
export type CitySearchResult = components['schemas']['CitySearchResult'];
export type CitySearchResponse = components['schemas']['CitySearchResponse'];
