import type { paths } from '$lib/api/v3/openapi';
import type { SearchV3Bucket, SearchV3Limits } from './SearchQueryKeyFactory';

// Where a unified search looks: the library and MusicBrainz, or the library
// alone.
export type SearchV3Scope = NonNullable<
	NonNullable<paths['/api/v3/search']['get']['parameters']['query']>['scope']
>;

export function unifiedSearchQuery(
	query: string,
	limits: SearchV3Limits,
	buckets: SearchV3Bucket[] | null,
	scope: SearchV3Scope
) {
	return {
		q: query.trim(),
		limit_artists: limits.artists,
		limit_albums: limits.albums,
		limit_tracks: limits.tracks,
		...(buckets ? { buckets: buckets.join(',') } : {}),
		...(scope === 'all' ? {} : { scope })
	};
}
