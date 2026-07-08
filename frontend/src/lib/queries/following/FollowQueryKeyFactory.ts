// userId scopes every key (AMU-5): without it the persisted cache leaks one
// user's follows to another on a shared browser.
export const FollowQueryKeyFactory = {
	statusPrefix: ['follow', 'status'] as const,
	followingPrefix: ['following'] as const,
	status: (mbid: string, userId: string | undefined) =>
		[...FollowQueryKeyFactory.statusPrefix, mbid, userId ?? 'anon'] as const,
	artists: (userId: string | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'artists', userId ?? 'anon'] as const,
	recentReleases: (
		userId: string | undefined,
		days: number,
		limit: number,
		includeOwned: boolean
	) =>
		[
			...FollowQueryKeyFactory.followingPrefix,
			'recent-releases',
			userId ?? 'anon',
			days,
			limit,
			includeOwned
		] as const,
	newReleasesUnseen: (userId: string | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'new-releases-unseen', userId ?? 'anon'] as const,
	concerts: (userId: string | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'concerts', userId ?? 'anon'] as const,
	concertCities: (userId: string | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'concert-cities', userId ?? 'anon'] as const,
	concertsUnseen: (userId: string | undefined) =>
		[...FollowQueryKeyFactory.followingPrefix, 'concerts-unseen', userId ?? 'anon'] as const,
	citySearch: (userId: string | undefined, q: string) =>
		[...FollowQueryKeyFactory.followingPrefix, 'city-search', userId ?? 'anon', q] as const,
	// admin queue is global (not per-user) - admins review every pending grant
	adminApprovals: () => [...FollowQueryKeyFactory.followingPrefix, 'admin-approvals'] as const,
	// bulk "Lidarr Import" approval cards, nested under admin-approvals so its prefix
	// invalidation sweeps both (LidarrImport D3)
	adminApprovalBatches: () =>
		[...FollowQueryKeyFactory.followingPrefix, 'admin-approvals', 'batches'] as const
};
