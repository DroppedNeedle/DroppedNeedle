import { key, userScope } from '../keys';

type UserId = string | null | undefined;

// Follows, releases, concerts and the approval queue are read per caller.
export const FollowQueryKeyFactory = {
	following: (userId: UserId) => key(userScope(userId), 'following'),
	status: (userId: UserId, mbid: string) => key(userScope(userId), 'follow', 'status', mbid),
	artists: (userId: UserId) => [...FollowQueryKeyFactory.following(userId), 'artists'] as const,
	recentReleases: (userId: UserId, days: number, limit: number, includeOwned: boolean) =>
		[
			...FollowQueryKeyFactory.following(userId),
			'recent-releases',
			days,
			limit,
			includeOwned
		] as const,
	newReleasesUnseen: (userId: UserId) =>
		[...FollowQueryKeyFactory.following(userId), 'new-releases-unseen'] as const,
	concerts: (userId: UserId) => [...FollowQueryKeyFactory.following(userId), 'concerts'] as const,
	concertCities: (userId: UserId) =>
		[...FollowQueryKeyFactory.following(userId), 'concert-cities'] as const,
	concertsUnseen: (userId: UserId) =>
		[...FollowQueryKeyFactory.following(userId), 'concerts-unseen'] as const,
	citySearch: (userId: UserId, q: string) =>
		[...FollowQueryKeyFactory.following(userId), 'city-search', q] as const,
	// The admin queue lists every pending grant; its count and the bulk Lidarr
	// import cards nest under it, so one invalidation refreshes all three.
	adminApprovals: (userId: UserId) =>
		[...FollowQueryKeyFactory.following(userId), 'admin-approvals'] as const,
	adminApprovalBatches: (userId: UserId) =>
		[...FollowQueryKeyFactory.adminApprovals(userId), 'batches'] as const,
	pendingApprovalCount: (userId: UserId) =>
		[...FollowQueryKeyFactory.adminApprovals(userId), 'count'] as const
};
