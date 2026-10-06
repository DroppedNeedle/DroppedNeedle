export interface LocalV3AlbumsParams {
	limit: number;
	offset: number;
	sort: string;
	order: 'asc' | 'desc';
	q?: string;
	decade?: number;
}

export interface LocalV3PageParams {
	limit?: number;
	offset?: number;
}

import { userIdSegment } from '../userKeySegment';

// Local-library keys. The card shapes carry no per-caller fields, so these
// stay shared; all nest under the local root for prefix invalidation. The
// download-access bit is per viewer, so that key carries the user id.
export const LOCAL_KEYS = {
	root: ['local'] as const,
	downloadAccess: (userId: string | null | undefined) =>
		[...LOCAL_KEYS.root, 'download-access', userIdSegment(userId)] as const,
	albums: (params: LocalV3AlbumsParams) => [...LOCAL_KEYS.root, 'albums', params] as const,
	recent: (limit: number | null) => [...LOCAL_KEYS.root, 'recent', limit] as const,
	suggestions: (limit: number, decade: number | null) =>
		[...LOCAL_KEYS.root, 'suggestions', limit, decade] as const,
	search: (term: string, limit: number | null) =>
		[...LOCAL_KEYS.root, 'search', term, limit] as const,
	decades: () => [...LOCAL_KEYS.root, 'decades'] as const,
	// Totals only: the stats view also carries per-caller favorite counts,
	// which no local consumer reads, so this key stays shared.
	stats: () => [...LOCAL_KEYS.root, 'stats'] as const,
	albumMatch: (mbid: string, page: LocalV3PageParams) =>
		[...LOCAL_KEYS.root, 'album-match', mbid, page] as const
};
