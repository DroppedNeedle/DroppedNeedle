import { userIdSegment } from '$lib/queries/userKeySegment';

// Config is the single admin-global connection (user-agnostic key). The candidates
// query is user-scoped (already_following differs per user), so its key carries the
// userId segment - without it the IndexedDB-persisted cache would leak one user's
// annotations to another on a shared browser.
export const LidarrImportQueryKeyFactory = {
	prefix: ['lidarr-import'] as const,
	config: () => [...LidarrImportQueryKeyFactory.prefix, 'config'] as const,
	candidates: (userId: string | null | undefined) =>
		[...LidarrImportQueryKeyFactory.prefix, 'candidates', userIdSegment(userId)] as const
};
