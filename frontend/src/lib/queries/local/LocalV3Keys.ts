import { key, SHARED, userScope } from '../keys';
import type { LibraryV3AlbumsParams } from '../library/LibraryQueryKeyFactory';

export type LocalV3AlbumsParams = Pick<
	LibraryV3AlbumsParams,
	'limit' | 'sort' | 'order' | 'q' | 'decade'
>;

export interface LocalV3PageParams {
	limit?: number;
	offset?: number;
}

type UserId = string | null | undefined;

// Local-library keys. The listening room's lists keep only the card fields,
// which carry nothing per caller, so they are shared. The download-access
// bit and the stats (with the caller's favourite counts) are the caller's.
export const LOCAL_KEYS = {
	feature: 'local',
	downloadAccess: (userId: UserId) => key(userScope(userId), 'local', 'download-access'),
	albums: (params: LocalV3AlbumsParams) => key(SHARED, 'local', 'albums', params),
	recent: (limit: number | null) => key(SHARED, 'local', 'recent', limit),
	suggestions: (limit: number, decade: number | null) =>
		key(SHARED, 'local', 'suggestions', limit, decade),
	search: (term: string) => key(SHARED, 'local', 'search', term),
	decades: () => key(SHARED, 'local', 'decades'),
	stats: (userId: UserId) => key(userScope(userId), 'local', 'stats'),
	albumMatch: (mbid: string, page: LocalV3PageParams) =>
		key(SHARED, 'local', 'album-match', mbid, page)
};
