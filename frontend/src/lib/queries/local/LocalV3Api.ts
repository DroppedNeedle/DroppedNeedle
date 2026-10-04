import { v3 } from '$lib/api/v3/endpoint';
import type { LocalV3AlbumsParams, LocalV3PageParams } from './LocalV3Keys';

// /api/v3 local-library URLs, built through the typed registry: every
// template is a literal the contract-coverage gate verifies against the
// generated spec. Nothing outside this feature imports them.
export const LocalV3Api = {
	albums: (params: LocalV3AlbumsParams) =>
		v3('/api/v3/local-library/albums', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.decade !== undefined ? { decade: params.decade } : {})
			}
		}),
	recent: (limit: number | null) =>
		limit === null
			? v3('/api/v3/local-library/recent')
			: v3('/api/v3/local-library/recent', { query: { limit } }),
	suggestions: (limit: number, decade: number | null) =>
		v3('/api/v3/local-library/suggestions', {
			query: decade === null ? { limit } : { limit, decade }
		}),
	search: (term: string, limit: number | null) =>
		v3('/api/v3/local-library/search', {
			query: limit === null ? { q: term } : { q: term, limit }
		}),
	decades: () => v3('/api/v3/local-library/decades'),
	stats: () => v3('/api/v3/library/stats'),
	albumMatch: (mbid: string, params: LocalV3PageParams) =>
		v3('/api/v3/local-library/albums/match/{mbid}', {
			path: { mbid },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		})
};
