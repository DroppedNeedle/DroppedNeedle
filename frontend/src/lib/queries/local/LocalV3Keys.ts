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

// v3 local-library keys. The v3 card shapes carry no per-caller fields, so
// these stay shared; all nest under the local root for prefix invalidation.
export const LOCAL_V3_KEYS = {
	root: ['local', 'v3'] as const,
	albums: (params: LocalV3AlbumsParams) => [...LOCAL_V3_KEYS.root, 'albums', params] as const,
	recent: (limit: number | null) => [...LOCAL_V3_KEYS.root, 'recent', limit] as const,
	suggestions: (limit: number, decade: number | null) =>
		[...LOCAL_V3_KEYS.root, 'suggestions', limit, decade] as const,
	search: (term: string, limit: number | null) =>
		[...LOCAL_V3_KEYS.root, 'search', term, limit] as const,
	decades: () => [...LOCAL_V3_KEYS.root, 'decades'] as const,
	// Totals only: the stats view also carries per-caller favorite counts,
	// which no local consumer reads, so this key stays shared.
	stats: () => [...LOCAL_V3_KEYS.root, 'stats'] as const,
	albumMatch: (mbid: string, page: LocalV3PageParams) =>
		[...LOCAL_V3_KEYS.root, 'album-match', mbid, page] as const
};
