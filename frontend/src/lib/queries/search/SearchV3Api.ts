import { v3 } from '$lib/api/v3/endpoint';
import type { SearchV3Bucket, SearchV3Limits } from './SearchQueryKeyFactory';

// /api/v3 search URLs, built through the typed registry: every template is
// a literal the contract-coverage gate verifies against the generated spec.
// Nothing outside this feature imports them.
export const SearchV3Api = {
	unified: (query: string, limits: SearchV3Limits, buckets: SearchV3Bucket[] | null) =>
		v3('/api/v3/search', {
			query: {
				q: query.trim(),
				limit_artists: limits.artists,
				limit_albums: limits.albums,
				limit_tracks: limits.tracks,
				...(buckets ? { buckets: buckets.join(',') } : {})
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
