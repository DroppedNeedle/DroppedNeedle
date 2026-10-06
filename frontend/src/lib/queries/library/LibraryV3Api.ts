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
	lyrics: (trackId: string) => v3('/api/v3/library/tracks/{id}/lyrics', { path: { id: trackId } }),
	recentlyAdded: (limit: number) => v3('/api/v3/library/recently-added', { query: { limit } }),
	stats: () => v3('/api/v3/library/stats'),
	editionPin: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/edition-pin', { path: { album_id: albumId } }),
	approveReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/approve', { path: { id: reviewId } }),
	rejectReview: (reviewId: string) =>
		v3('/api/v3/library/reviews/{id}/reject', { path: { id: reviewId } }),
	operation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}', { path: { job_id: jobId } }),
	pauseOperation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/pause', { path: { job_id: jobId } }),
	resumeOperation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/resume', { path: { job_id: jobId } }),
	stopOperation: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/stop', { path: { job_id: jobId } }),
	operationCandidate: (jobId: string) =>
		v3('/api/v3/library/operations/{job_id}/candidate', { path: { job_id: jobId } }),
	reidentifyAlbum: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/reidentify', { path: { album_id: albumId } }),
	reidentificationReleases: (
		albumId: string,
		params: { title: string; artist: string; limit: number; offset: number }
	) =>
		v3('/api/v3/library/albums/{album_id}/reidentification/releases', {
			path: { album_id: albumId },
			query: params
		}),
	undoAutomaticEdition: (albumId: string) =>
		v3('/api/v3/library/albums/{album_id}/undo-automatic-edition', {
			path: { album_id: albumId }
		}),
	scanRuns: () => v3('/api/v3/library/scan/runs'),
	scanRun: (runId: string) => v3('/api/v3/library/scan/runs/{id}', { path: { id: runId } }),
	currentScanRuns: () => v3('/api/v3/library/scan/runs/current'),
	scanRunHistory: (limit: number, cursor?: string) =>
		v3('/api/v3/library/scan/runs/history', {
			query: { limit, ...(cursor ? { cursor } : {}) }
		}),
	scanRunEstimate: (scopeIds: string[]) =>
		v3('/api/v3/library/scan/runs/estimate', {
			query: scopeIds.length ? { scope_ids: scopeIds } : {}
		}),
	scanRunFailures: (runId: string, limit: number, cursor?: number) =>
		v3('/api/v3/library/scan/runs/{id}/failures', {
			path: { id: runId },
			query: { limit, ...(cursor !== undefined ? { cursor } : {}) }
		}),
	pauseScanRun: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/pause', { path: { id: runId } }),
	resumeScanRun: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/resume', { path: { id: runId } }),
	stopScanRun: (runId: string) =>
		v3('/api/v3/library/scan/runs/{id}/stop', { path: { id: runId } }),
	activity: () => v3('/api/v3/library/activity'),
	pauseIdentification: () => v3('/api/v3/library/identification/pause'),
	resumeIdentification: () => v3('/api/v3/library/identification/resume'),
	schedule: () => v3('/api/v3/settings/library/schedule'),
	settings: () => v3('/api/v3/settings/library'),
	policyTree: () => v3('/api/v3/settings/library/policy-tree'),
	policyImpact: () => v3('/api/v3/settings/library/policy-impact'),
	policyApplyPreview: () => v3('/api/v3/settings/library/policy-apply-preview'),
	pathMapping: () => v3('/api/v3/settings/library/path-mapping'),
	restorableRoots: () => v3('/api/v3/settings/library/restorable-roots'),
	restoreRoots: () => v3('/api/v3/settings/library/restore-roots')
};
