import { userIdSegment } from '$lib/queries/userKeySegment';

// userId scopes every key (AMU-5): without it the persisted cache leaks one
// user's follows to another on a shared browser.
export const FollowQueryKeyFactory = {
	statusPrefix: ['follow', 'status'] as const,
	followingPrefix: ['following'] as const,
	status: (mbid: string, userId: string | null | undefined) =>
		[...FollowQueryKeyFactory.statusPrefix, mbid, userIdSegment(userId)] as const,
	artists: (userId: string | null | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'artists', userIdSegment(userId)] as const,
	recentReleases: (
		userId: string | null | undefined,
		days: number,
		limit: number,
		includeOwned: boolean
	) =>
		[
			...FollowQueryKeyFactory.followingPrefix,
			'recent-releases',
			userIdSegment(userId),
			days,
			limit,
			includeOwned
		] as const,
	newReleasesUnseen: (userId: string | null | undefined) =>
		[
			...FollowQueryKeyFactory.followingPrefix,
			'new-releases-unseen',
			userIdSegment(userId)
		] as const,
	concerts: (userId: string | null | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'concerts', userIdSegment(userId)] as const,
	concertCities: (userId: string | null | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'concert-cities', userIdSegment(userId)] as const,
	concertsUnseen: (userId: string | null | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'concerts-unseen', userIdSegment(userId)] as const,
	citySearch: (userId: string | null | undefined, q: string) =>
		[...FollowQueryKeyFactory.followingPrefix, 'city-search', userIdSegment(userId), q] as const,
	// admin queue is global (not per-user) - admins review every pending grant
	adminApprovals: () => [...FollowQueryKeyFactory.followingPrefix, 'admin-approvals'] as const,
	// bulk "Lidarr Import" approval cards, nested under admin-approvals so its prefix
	// invalidation sweeps both (LidarrImport D3)
	adminApprovalBatches: () =>
		[...FollowQueryKeyFactory.followingPrefix, 'admin-approvals', 'batches'] as const,
	pendingApprovalCount: (userId: string | null | undefined) =>
		[
			...FollowQueryKeyFactory.followingPrefix,
			'admin-approvals',
			'count',
			userIdSegment(userId)
		] as const
};
