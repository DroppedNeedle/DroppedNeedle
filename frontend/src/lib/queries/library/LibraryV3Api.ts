import { v3 } from '$lib/api/v3/endpoint';
import type {
	LibraryV3AlbumsParams,
	LibraryV3ArtistsParams,
	LibraryV3PageParams,
	LibraryV3TracksParams
} from './LibraryQueryKeyFactory';

// /api/v3 library URLs, built through the typed registry: every template is
// a literal the contract-coverage gate verifies against the generated spec.
// Nothing outside this feature imports them.
export const LibraryV3Api = {
	albums: (params: LibraryV3AlbumsParams) =>
		v3('/api/v3/library/albums', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.artistId ? { artist_id: params.artistId } : {}),
				...(params.decade !== undefined ? { decade: params.decade } : {}),
				...(params.format ? { format: params.format } : {})
			}
		}),
	artists: (params: LibraryV3ArtistsParams) =>
		v3('/api/v3/library/artists', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.scope ? { scope: params.scope } : {})
			}
		}),
	albumDetail: (albumId: string) => v3('/api/v3/library/albums/{id}', { path: { id: albumId } }),
	albumCopies: (albumId: string) =>
		v3('/api/v3/library/albums/{id}/copies', { path: { id: albumId } }),
	albumTracks: (albumId: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/albums/{id}/tracks', {
			path: { id: albumId },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	artistDetail: (artistId: string) =>
		v3('/api/v3/library/artists/{id}', { path: { id: artistId } }),
	artistAlbums: (artistId: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/artists/{id}/albums', {
			path: { id: artistId },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	artistAppearances: (artistId: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/artists/{id}/appearances', {
			path: { id: artistId },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	tracks: (params: LibraryV3TracksParams) =>
		v3('/api/v3/library/tracks', {
			query: {
				limit: params.limit,
				offset: params.offset,
				sort: params.sort,
				order: params.order,
				...(params.q ? { q: params.q } : {}),
				...(params.albumId ? { album_id: params.albumId } : {}),
				...(params.artistId ? { artist_id: params.artistId } : {}),
				...(params.genre ? { genre: params.genre } : {})
			}
		}),
	trackDetail: (trackId: string) => v3('/api/v3/library/tracks/{id}', { path: { id: trackId } }),
	lyrics: (trackId: string) => v3('/api/v3/library/tracks/{id}/lyrics', { path: { id: trackId } }),
	genres: () => v3('/api/v3/library/genres'),
	genreTracks: (name: string, params: LibraryV3PageParams) =>
		v3('/api/v3/library/genres/{name}/tracks', {
			path: { name },
			query: {
				...(params.limit !== undefined ? { limit: params.limit } : {}),
				...(params.offset !== undefined ? { offset: params.offset } : {})
			}
		}),
	recentlyAdded: (limit: number) => v3('/api/v3/library/recently-added', { query: { limit } }),
	stats: () => v3('/api/v3/library/stats'),
	reviews: (albumId: string) => v3('/api/v3/library/reviews', { query: { album_id: albumId } }),
	editionPin: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition-pin', { path: { album_id: albumId } }),
	identify: () => v3('/api/v3/library/identify'),
	managePreview: () => v3('/api/v3/library/manage/preview'),
	manageApply: () => v3('/api/v3/library/manage/apply'),
	manageUndo: () => v3('/api/v3/library/manage/undo'),
	baselineRestore: () => v3('/api/v3/library/manage/baseline/restore'),
	approveReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/approve', { path: { id: reviewId } }),
	rejectReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/reject', { path: { id: reviewId } }),
	scan: () => v3('/api/v3/library/scan'),
	scanRuns: () => v3('/api/v3/library/scan/runs'),
	scanRun: (runId: string) => v3('/api/v3/library/scan/runs/{id}', { path: { id: runId } }),
	roots: () => v3('/api/v3/library/roots')
};
