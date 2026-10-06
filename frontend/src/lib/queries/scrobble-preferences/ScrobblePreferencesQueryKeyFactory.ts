import { userIdSegment } from '$lib/queries/userKeySegment';

// userId dimension is mandatory (AMU-5): prefs are per-user and must not leak
// across users on a shared browser
export const ScrobblePreferencesQueryKeyFactory = {
	prefix: ['me', 'scrobble-preferences'] as const,
	get: (userId: string | null | undefined) =>
		[...ScrobblePreferencesQueryKeyFactory.prefix, userIdSegment(userId)] as const,
	// admin-only queue (no user dimension, matching FollowQueryKeyFactory.adminApprovals)
	personalMixApprovals: () =>
		[...ScrobblePreferencesQueryKeyFactory.prefix, 'personal-mix-approvals'] as const
};
