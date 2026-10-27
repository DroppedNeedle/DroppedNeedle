import { v3 } from '$lib/api/v3/endpoint';
import type { paths } from '$lib/api/v3/openapi';
import type { SearchV3Bucket, SearchV3Limits } from './SearchQueryKeyFactory';

// Where a unified search looks: the library and MusicBrainz, or the library
// alone.
export type SearchV3Scope = NonNullable<
	NonNullable<paths['/api/v3/search']['get']['parameters']['query']>['scope']
>;

// /api/v3 search URLs, built through the typed registry: every template is
// a literal the contract-coverage gate verifies against the generated spec.
// The album bucket also backs the drop-import and YouTube album pickers.
export const SearchV3Api = {
	unified: (
		query: string,
		limits: SearchV3Limits,
		buckets: SearchV3Bucket[] | null,
		scope: SearchV3Scope = 'all'
	) =>
		v3('/api/v3/search', {
			query: {
				q: query.trim(),
				limit_artists: limits.artists,
				limit_albums: limits.albums,
				limit_tracks: limits.tracks,
				...(buckets ? { buckets: buckets.join(',') } : {}),
				...(scope === 'all' ? {} : { scope })
			}
		}),
	bucket: (bucket: SearchV3Bucket, query: string, limit: number, offset: number) =>
		v3('/api/v3/search/{bucket}', {
			path: { bucket },
			query: { q: query.trim(), limit, offset }
		}),
	suggest: (query: string, limit: number) =>
		v3('/api/v3/search/suggest', { query: { q: query.trim(), limit } }),
	enrichBatch: () => v3('/api/v3/search/enrich/batch')
};
