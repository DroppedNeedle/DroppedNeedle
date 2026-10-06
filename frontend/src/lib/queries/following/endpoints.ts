import { v3 } from '$lib/api/v3/endpoint';

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

export const CONCERT_ENDPOINTS = {
	concerts: () => v3('/api/v3/following/concerts'),
	concertCities: () => v3('/api/v3/following/concerts/cities'),
	concertCitySearch: (q: string) => v3('/api/v3/following/concerts/city-search', { query: { q } }),
	concertsUnseenCount: () => v3('/api/v3/following/concerts/unseen-count'),
	markConcertsSeen: () => v3('/api/v3/following/concerts/seen')
} as const;
