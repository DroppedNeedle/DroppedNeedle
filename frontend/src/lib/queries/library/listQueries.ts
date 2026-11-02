import type {
	LibraryReviewParams,
	LibraryV3AlbumsParams,
	LibraryV3ArtistsParams,
	LibraryV3CursorParams,
	LibraryV3PageParams,
	LibraryV3TracksParams
} from './LibraryQueryKeyFactory';

/** A review list filter as the server's query fields (paging aside). */
export function reviewFilterQuery(params: Omit<LibraryReviewParams, 'limit'>) {
	return {
		...(params.search ? { search: params.search } : {}),
		...(params.reasonCode ? { reason_code: params.reasonCode } : {}),
		...(params.rootId ? { root_id: params.rootId } : {}),
		...(params.policy ? { policy: params.policy } : {}),
		...(params.jobState ? { job_state: params.jobState } : {}),
		...(params.hideActiveJobs ? { exclude_active_jobs: true } : {}),
		...(params.createdFrom !== undefined ? { created_from: params.createdFrom } : {}),
		...(params.createdTo !== undefined ? { created_to: params.createdTo } : {}),
		...(params.withCandidates ? { with_candidates: true } : {}),
		...(params.sort ? { sort: params.sort } : {})
	};
}

/** Identification reviews, one page, pending unless the filter names a state. */
export function reviewsQuery(params: LibraryReviewParams, cursor?: string) {
	return {
		state: params.state ?? ('pending' as const),
		limit: params.limit,
		...(cursor ? { cursor } : {}),
		...reviewFilterQuery(params)
	};
}

// The long lists read by cursor: `after` is the `next` of the page before.
export function albumsQuery(params: LibraryV3AlbumsParams, after: string | null) {
	return {
		limit: params.limit,
		...(after ? { after } : {}),
		...(params.sort ? { sort: params.sort } : {}),
		...(params.order ? { order: params.order } : {}),
		...(params.q ? { q: params.q } : {}),
		...(params.artistId ? { artist_id: params.artistId } : {}),
		...(params.decade !== undefined ? { decade: params.decade } : {}),
		...(params.format ? { format: params.format } : {}),
		...(params.favorites ? { favorites: true } : {})
	};
}

export function artistsQuery(params: LibraryV3ArtistsParams, after: string | null) {
	return {
		limit: params.limit,
		...(after ? { after } : {}),
		...(params.sort ? { sort: params.sort } : {}),
		...(params.order ? { order: params.order } : {}),
		...(params.q ? { q: params.q } : {}),
		...(params.scope ? { scope: params.scope } : {}),
		...(params.favorites ? { favorites: true } : {})
	};
}

export function tracksQuery(params: LibraryV3TracksParams, after: string | null) {
	return {
		limit: params.limit,
		...(after ? { after } : {}),
		...(params.sort ? { sort: params.sort } : {}),
		...(params.order ? { order: params.order } : {}),
		...(params.q ? { q: params.q } : {}),
		...(params.albumId ? { album_id: params.albumId } : {}),
		...(params.artistId ? { artist_id: params.artistId } : {}),
		...(params.genre ? { genre: params.genre } : {}),
		...(params.favorites ? { favorites: true } : {})
	};
}

export function pageQuery(params: LibraryV3PageParams) {
	return {
		...(params.limit !== undefined ? { limit: params.limit } : {}),
		...(params.offset !== undefined ? { offset: params.offset } : {})
	};
}

export function cursorQuery(params: LibraryV3CursorParams) {
	return {
		...(params.limit !== undefined ? { limit: params.limit } : {}),
		...(params.after ? { after: params.after } : {})
	};
}
