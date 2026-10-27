import type { LibraryV3AlbumsParams } from '../library/LibraryQueryKeyFactory';
import { userIdSegment } from '../userKeySegment';

export type LocalV3AlbumsParams = Pick<
	LibraryV3AlbumsParams,
	'limit' | 'sort' | 'order' | 'q' | 'decade'
>;

export interface LocalV3PageParams {
	limit?: number;
	offset?: number;
}

// Local-library keys. The listening room's queries keep only the card
// fields, which carry nothing per caller, so these stay shared; all nest
// under the local root for prefix invalidation. The download-access bit is
// per viewer, so that key carries the user id.
export const LOCAL_KEYS = {
	root: ['local'] as const,
	downloadAccess: (userId: string | null | undefined) =>
		[...LOCAL_KEYS.root, 'download-access', userIdSegment(userId)] as const,
	albums: (params: LocalV3AlbumsParams) => [...LOCAL_KEYS.root, 'albums', params] as const,
	recent: (limit: number | null) => [...LOCAL_KEYS.root, 'recent', limit] as const,
	suggestions: (limit: number, decade: number | null) =>
		[...LOCAL_KEYS.root, 'suggestions', limit, decade] as const,
	search: (term: string) => [...LOCAL_KEYS.root, 'search', term] as const,
	decades: () => [...LOCAL_KEYS.root, 'decades'] as const,
	// Totals only: the stats view also carries per-caller favorite counts,
	// which no local consumer reads, so this key stays shared.
	stats: () => [...LOCAL_KEYS.root, 'stats'] as const,
	albumMatch: (mbid: string, page: LocalV3PageParams) =>
		[...LOCAL_KEYS.root, 'album-match', mbid, page] as const
};
