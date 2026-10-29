import { ADMIN, key, userScope } from '../keys';

// The connection is the one admin setting. The candidates say which artists
// the caller already follows, so they are the caller's.
export const LidarrImportQueryKeyFactory = {
	feature: 'lidarr-import',
	config: () => key(ADMIN, 'lidarr-import', 'config'),
	candidates: (userId: string | null | undefined) =>
		key(userScope(userId), 'lidarr-import', 'candidates')
};
