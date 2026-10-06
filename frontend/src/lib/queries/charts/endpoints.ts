import { v3 } from '$lib/api/v3/endpoint';

/** The three ranked chart pages under /home. */
export type ChartKind = 'trending-artists' | 'popular-albums' | 'your-top-albums';
export type ChartRange = 'this_week' | 'this_month' | 'this_year' | 'all_time';
export type ChartSource = 'listenbrainz' | 'lastfm';

export interface ChartPageParams {
	range: ChartRange;
	limit: number;
	offset: number;
	source: ChartSource | null;
}

function chartQuery({ range, limit, offset, source }: ChartPageParams) {
	return source ? { range, limit, offset, source } : { range, limit, offset };
}

export const HOME_ENDPOINTS = {
	home: () => v3('/api/v3/home'),
	integrationStatus: () => v3('/api/v3/home/integration-status'),
	genre: (genre: string, limit: number, artistOffset: number, albumOffset: number) =>
		v3('/api/v3/home/genre/{genre_name}', {
			path: { genre_name: genre },
			query: { limit, artist_offset: artistOffset, album_offset: albumOffset }
		}),
	trendingArtists: (params: ChartPageParams) =>
		v3('/api/v3/home/trending/artists', { query: chartQuery(params) }),
	popularAlbums: (params: ChartPageParams) =>
		v3('/api/v3/home/popular/albums', { query: chartQuery(params) }),
	yourTopAlbums: (params: ChartPageParams) =>
		v3('/api/v3/home/your-top/albums', { query: chartQuery(params) })
} as const;
