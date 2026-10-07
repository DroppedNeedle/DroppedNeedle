import { userIdSegment } from '../userKeySegment';

export type LibraryV3UserId = string | null | undefined;

export interface LibraryV3AlbumsParams {
	limit: number;
	offset: number;
	sort: 'name' | 'date_added' | 'year' | 'artist';
	order: 'asc' | 'desc';
	q?: string;
	artistId?: string;
	decade?: number;
	format?: string;
}

export interface LibraryV3ArtistsParams {
	limit: number;
	offset: number;
	sort: 'name' | 'album_count' | 'appearance_count' | 'date_added';
	order: 'asc' | 'desc';
	q?: string;
	scope?: 'all' | 'album_artists' | 'contributors';
}

export interface LibraryV3TracksParams {
	limit: number;
	offset: number;
	sort: 'title' | 'date_added';
	order: 'asc' | 'desc';
	q?: string;
	albumId?: string;
	artistId?: string;
	genre?: string;
}

export interface LibraryV3PageParams {
	limit?: number;
	offset?: number;
}

export const LibraryQueryKeyFactory = {
	all: ['library'] as const,
	// Catalog keys. Album and artist views carry per-caller favorite flags, so
	// every catalog key carries the userId segment (a missing one would leak
	// one user's flags to the next through the persisted cache).
	catalog: {
		root: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.all, 'catalog', userIdSegment(userId)] as const,
		albums: (userId: LibraryV3UserId, params: LibraryV3AlbumsParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'albums', params] as const,
		artists: (userId: LibraryV3UserId, params: LibraryV3ArtistsParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artists', params] as const,
		artistThumbs: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artist-thumbs'] as const,
		albumDetail: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-detail', albumId] as const,
		albumCopies: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-copies', albumId] as const,
		albumTracks: (userId: LibraryV3UserId, albumId: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-tracks', albumId, params] as const,
		artistDetail: (userId: LibraryV3UserId, artistId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artist-detail', artistId] as const,
		artistAlbums: (userId: LibraryV3UserId, artistId: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'artist-albums', artistId, params] as const,
		artistAppearances: (userId: LibraryV3UserId, artistId: string, params: LibraryV3PageParams) =>
			[
				...LibraryQueryKeyFactory.catalog.root(userId),
				'artist-appearances',
				artistId,
				params
			] as const,
		recentlyAddedPrefix: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'recently-added'] as const,
		recentlyAdded: (userId: LibraryV3UserId, limit: number) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'recently-added', limit] as const,
		search: (userId: LibraryV3UserId, q: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'search', q] as const,
		albumSearch: (userId: LibraryV3UserId, q: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'album-search', q] as const,
		stats: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'stats'] as const,
		edition: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'edition', albumId] as const,
		unconfirmed: (userId: LibraryV3UserId, state: string, offset: number) =>
			[...LibraryQueryKeyFactory.catalog.root(userId), 'unconfirmed', state, offset] as const
	},
	activityPrefix: () => [...LibraryQueryKeyFactory.all, 'activity'] as const,
	activity: (userId: LibraryV3UserId) =>
		[...LibraryQueryKeyFactory.activityPrefix(), userIdSegment(userId)] as const,
	membership: (userId: LibraryV3UserId, albumIds: string[]) =>
		[
			...LibraryQueryKeyFactory.all,
			'membership',
			userIdSegment(userId),
			[...albumIds].sort()
		] as const,
	operationsPrefix: () => [...LibraryQueryKeyFactory.all, 'operations'] as const,
	currentRuns: () => [...LibraryQueryKeyFactory.operationsPrefix(), 'current-runs'] as const,
	runHistory: (cursor: string | undefined) =>
		[...LibraryQueryKeyFactory.operationsPrefix(), 'run-history', cursor ?? 'first'] as const,
	run: (runId: string) => [...LibraryQueryKeyFactory.operationsPrefix(), 'run', runId] as const,
	runFailures: (runId: string) =>
		[...LibraryQueryKeyFactory.operationsPrefix(), 'run-failures', runId] as const,
	runEstimate: (scopeIds: string[]) =>
		[...LibraryQueryKeyFactory.operationsPrefix(), 'estimate', [...scopeIds].sort()] as const,
	reviewsPrefix: () => [...LibraryQueryKeyFactory.all, 'reviews'] as const,
	reviews: (params: object) => [...LibraryQueryKeyFactory.reviewsPrefix(), params] as const,
	review: (reviewId: string) =>
		[...LibraryQueryKeyFactory.reviewsPrefix(), 'detail', reviewId] as const,
	policyPrefix: () => [...LibraryQueryKeyFactory.all, 'policy'] as const,
	targetSettings: () => [...LibraryQueryKeyFactory.policyPrefix(), 'settings'] as const,
	policyTree: () => [...LibraryQueryKeyFactory.policyPrefix(), 'tree'] as const,
	pathMapping: () => [...LibraryQueryKeyFactory.policyPrefix(), 'path-mapping'] as const,
	restorableRoots: () => [...LibraryQueryKeyFactory.policyPrefix(), 'restorable-roots'] as const,
	repairsPrefix: () => [...LibraryQueryKeyFactory.operationsPrefix(), 'repairs'] as const,
	repairs: (cursor: string | undefined) =>
		[...LibraryQueryKeyFactory.repairsPrefix(), 'history', cursor ?? 'first'] as const,
	repairEstimate: (rootIds: string[]) =>
		[...LibraryQueryKeyFactory.repairsPrefix(), 'estimate', [...rootIds].sort()] as const,
	repair: (jobId: string) => [...LibraryQueryKeyFactory.repairsPrefix(), 'detail', jobId] as const,
	repairFindings: (jobId: string, findingCategory: string, cursor: string | undefined) =>
		[
			...LibraryQueryKeyFactory.repairsPrefix(),
			'findings',
			jobId,
			findingCategory,
			cursor ?? 'first'
		] as const,
	identityPreparationsPrefix: (userId: LibraryV3UserId) =>
		[
			...LibraryQueryKeyFactory.operationsPrefix(),
			'identity-preparations',
			userIdSegment(userId)
		] as const,
	identityPreparations: (userId: LibraryV3UserId, cursor: string | undefined) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'history',
			cursor ?? 'first'
		] as const,
	identityPreparationEstimate: (userId: LibraryV3UserId, rootIds: string[]) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'estimate',
			[...rootIds].sort()
		] as const,
	identityPreparationFindings: (
		userId: LibraryV3UserId,
		jobId: string,
		findingCategory: string,
		cursor: string | undefined
	) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'findings',
			jobId,
			findingCategory,
			cursor ?? 'first'
		] as const,
	album: (mbid: string) => [...LibraryQueryKeyFactory.all, 'album', mbid] as const,
	reidentificationReleases: (
		userId: LibraryV3UserId,
		albumId: string,
		title: string,
		artist: string,
		offset: number
	) =>
		[
			...LibraryQueryKeyFactory.all,
			'reidentification-releases',
			userIdSegment(userId),
			albumId,
			title,
			artist,
			offset
		] as const,
	scanSchedule: () => [...LibraryQueryKeyFactory.all, 'scan-schedule'] as const
};
