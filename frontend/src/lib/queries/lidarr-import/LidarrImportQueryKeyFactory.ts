// The candidates query is user-dependent (already_following differs per user), so its
// key carries the userId segment - without it the IndexedDB-persisted cache would leak
// one user's annotations to another on a shared browser. Config + status are not
// user-specific (config is the single admin connection; status is a global boolean).
export const LidarrImportQueryKeyFactory = {
	prefix: ['lidarr-import'] as const,
	config: () => [...LidarrImportQueryKeyFactory.prefix, 'config'] as const,
	status: () => [...LidarrImportQueryKeyFactory.prefix, 'status'] as const,
	candidates: (userId: string | undefined) =>
		[...LidarrImportQueryKeyFactory.prefix, 'candidates', userId ?? 'anon'] as const
};
