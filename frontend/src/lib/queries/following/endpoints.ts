import { v3 } from '$lib/api/v3/endpoint';
import { API } from '$lib/constants';

// The current user is resolved server-side from the session cookie.
export const FOLLOW_ENDPOINTS = {
	status: (mbid: string) =>
		v3('/api/v3/artists/{artist_mbid}/follow-status', { path: { artist_mbid: mbid } }),
	setFollow: (mbid: string) =>
		v3('/api/v3/artists/{artist_mbid}/follow', { path: { artist_mbid: mbid } }),
	autoDownload: (mbid: string) =>
		v3('/api/v3/artists/{artist_mbid}/auto-download', { path: { artist_mbid: mbid } }),
	followedArtists: () => v3('/api/v3/following/artists'),
	recentReleases: () => v3('/api/v3/following/new-releases/recent'),
	newReleasesUnseenCount: () => v3('/api/v3/following/new-releases/unseen-count'),
	markNewReleasesSeen: () => v3('/api/v3/following/new-releases/seen'),
	adminApprovals: () => v3('/api/v3/requests/auto-download-approvals'),
	approve: (userId: string, mbid: string) =>
		v3('/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/approve', {
			path: { user_id: userId, artist_mbid: mbid }
		}),
	reject: (userId: string, mbid: string) =>
		v3('/api/v3/requests/auto-download-approvals/{user_id}/{artist_mbid}/reject', {
			path: { user_id: userId, artist_mbid: mbid }
		}),
	adminApprovalBatches: () => v3('/api/v3/requests/auto-download-approval-batches'),
	approveBatch: (batchId: string) =>
		v3('/api/v3/requests/auto-download-approval-batches/{batch_id}/approve', {
			path: { batch_id: batchId }
		}),
	rejectBatch: (batchId: string) =>
		v3('/api/v3/requests/auto-download-approval-batches/{batch_id}/reject', {
			path: { batch_id: batchId }
		})
} as const;

// Concerts and event cities have no v3 route yet; these stay on the old
// paths until the server serves them.
export const CONCERT_ENDPOINTS = {
	concerts: () => API.following.concerts(),
	concertCities: () => API.following.concertCities(),
	concertCitySearch: (q: string) => API.following.concertCitySearch(q),
	concertsUnseenCount: () => API.following.concertsUnseenCount(),
	markConcertsSeen: () => API.following.markConcertsSeen()
} as const;
