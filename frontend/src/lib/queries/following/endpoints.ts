import { v3 } from '$lib/api/v3/endpoint';
import { API } from '$lib/constants';

// the current user is resolved server-side from the session cookie
export const FOLLOW_ENDPOINTS = {
	status: (mbid: string) => API.artist.follow(mbid),
	setFollow: (mbid: string) => API.artist.follow(mbid),
	autoDownload: (mbid: string) => API.artist.autoDownload(mbid),
	followedArtists: () => API.following.artists(),
	recentReleases: (days: number, limit: number, includeOwned: boolean) =>
		API.following.recentReleases(days, limit, includeOwned),
	newReleasesUnseenCount: () => API.following.newReleasesUnseenCount(),
	markNewReleasesSeen: () => API.following.markNewReleasesSeen(),
	concerts: () => API.following.concerts(),
	concertCities: () => API.following.concertCities(),
	concertCitySearch: (q: string) => API.following.concertCitySearch(q),
	concertsUnseenCount: () => API.following.concertsUnseenCount(),
	markConcertsSeen: () => API.following.markConcertsSeen(),
	// v3 approval URLs (the follow rows above stay v1 for the follows
	// migration), built through the typed registry.
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
