import { tracksQuery } from '$lib/features/library/queries/listQueries';
import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import type { LibraryV3TracksParams } from './LibraryQueryKeyFactory';

/**
 * Track list orders the v3 catalog serves. `hearted` is the favourites' own
 * order, the newest heart first.
 */
export type LibraryTrackSort = 'recent' | 'title' | 'artist' | 'album' | 'random' | 'hearted';

/** One page of the track list; `next` reads the page after it. */
export type LibraryTrackPage = Pick<
	components['schemas']['TrackListPage'],
	'items' | 'total' | 'next'
>;

/** The track list's params for one order and search, the cursor aside. */
export function libraryTrackParams(
	limit: number,
	sort: LibraryTrackSort,
	q: string = '',
	favorites = false
): LibraryV3TracksParams {
	return {
		limit,
		...(sort === 'hearted'
			? {}
			: {
					sort: sort === 'recent' ? 'date_added' : sort,
					order: sort === 'recent' ? 'desc' : 'asc'
				}),
		...(q ? { q } : {}),
		...(favorites ? { favorites: true } : {})
	};
}

/** One page of the local catalog's tracks: the first, or the one `after` names. */
export function fetchLibraryTracks(
	params: LibraryV3TracksParams,
	after: string | null,
	signal?: AbortSignal
): Promise<LibraryTrackPage> {
	return api.v3.GET(
		v3('/api/v3/library/tracks', {
			query: tracksQuery(params, after)
		}),
		{ signal }
	);
}
