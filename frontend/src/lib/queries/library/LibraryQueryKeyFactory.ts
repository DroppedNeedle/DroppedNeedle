import { userIdSegment } from '../userKeySegment';

type AlbumSort = 'recent' | 'title' | 'artist';
type ArtistSort = 'name' | 'album_count' | 'appearance_count' | 'date_added';

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
	sort: 'name' | 'album_count' | 'date_added';
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
	// v3 catalog keys. Album and artist views carry per-caller favorite flags,
	// so every v3 key carries the userId segment (a missing one would leak one
	// user's flags to the next through the persisted cache).
	v3: {
		root: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.all, 'v3', userIdSegment(userId)] as const,
		albums: (userId: LibraryV3UserId, params: LibraryV3AlbumsParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'albums', params] as const,
		artists: (userId: LibraryV3UserId, params: LibraryV3ArtistsParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'artists', params] as const,
		artistThumbs: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'artist-thumbs'] as const,
		albumDetail: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'album-detail', albumId] as const,
		albumCopies: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'album-copies', albumId] as const,
		albumTracks: (userId: LibraryV3UserId, albumId: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'album-tracks', albumId, params] as const,
		artistDetail: (userId: LibraryV3UserId, artistId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'artist-detail', artistId] as const,
		artistAlbums: (userId: LibraryV3UserId, artistId: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'artist-albums', artistId, params] as const,
		artistAppearances: (userId: LibraryV3UserId, artistId: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'artist-appearances', artistId, params] as const,
		tracks: (userId: LibraryV3UserId, params: LibraryV3TracksParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'tracks', params] as const,
		trackDetail: (userId: LibraryV3UserId, trackId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'track-detail', trackId] as const,
		lyrics: (userId: LibraryV3UserId, trackId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'lyrics', trackId] as const,
		genres: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'genres'] as const,
		genreTracks: (userId: LibraryV3UserId, name: string, params: LibraryV3PageParams) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'genre-tracks', name, params] as const,
		recentlyAdded: (userId: LibraryV3UserId, limit: number) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'recently-added', limit] as const,
		stats: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'stats'] as const,
		reviews: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'reviews', albumId] as const,
		editionPin: (userId: LibraryV3UserId, albumId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'edition-pin', albumId] as const,
		scanRuns: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'scan-runs'] as const,
		scanRun: (userId: LibraryV3UserId, runId: string) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'scan-run', runId] as const,
		roots: (userId: LibraryV3UserId) =>
			[...LibraryQueryKeyFactory.v3.root(userId), 'roots'] as const
	},
	activityPrefix: () => [...LibraryQueryKeyFactory.all, 'activity'] as const,
	activity: (userId: string | undefined) =>
		[...LibraryQueryKeyFactory.activityPrefix(), userId ?? 'anonymous'] as const,
	membership: (userId: string | undefined, albumIds: string[]) =>
		[
			...LibraryQueryKeyFactory.all,
			'membership',
			userId ?? 'anonymous',
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
	identityPreparationsPrefix: (userId: string | undefined) =>
		[
			...LibraryQueryKeyFactory.operationsPrefix(),
			'identity-preparations',
			userId ?? 'anonymous'
		] as const,
	identityPreparations: (userId: string | undefined, cursor: string | undefined) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'history',
			cursor ?? 'first'
		] as const,
	identityPreparationEstimate: (userId: string | undefined, rootIds: string[]) =>
		[
			...LibraryQueryKeyFactory.identityPreparationsPrefix(userId),
			'estimate',
			[...rootIds].sort()
		] as const,
	identityPreparationFindings: (
		userId: string | undefined,
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
	albums: (page: number, sort: AlbumSort, q: string, format: string) =>
		[...LibraryQueryKeyFactory.all, 'albums', { page, sort, q, format }] as const,
	artists: (scope: string, sortBy: ArtistSort, sortOrder: string, q: string) =>
		[...LibraryQueryKeyFactory.all, 'artists', { scope, sortBy, sortOrder, q }] as const,
	album: (mbid: string) => [...LibraryQueryKeyFactory.all, 'album', mbid] as const,
	albumDetail: (albumId: string) =>
		[...LibraryQueryKeyFactory.all, 'album-detail', albumId] as const,
	editionConversionsPrefix: (userId: string | undefined) =>
		[...LibraryQueryKeyFactory.all, 'edition-conversions', userId ?? 'anonymous'] as const,
	editionConversion: (userId: string | undefined, jobId: string | null) =>
		[...LibraryQueryKeyFactory.editionConversionsPrefix(userId), jobId ?? 'inactive'] as const,
	reidentificationReleases: (
		userId: string | undefined,
		albumId: string,
		title: string,
		artist: string,
		offset: number
	) =>
		[
			...LibraryQueryKeyFactory.all,
			'reidentification-releases',
			userId ?? 'anonymous',
			albumId,
			title,
			artist,
			offset
		] as const,
	albumCopies: (albumId: string) =>
		[...LibraryQueryKeyFactory.all, 'album-copies', albumId] as const,
	artistDetail: (artistId: string) =>
		[...LibraryQueryKeyFactory.all, 'artist-detail', artistId] as const,
	artistAlbums: (artistId: string) =>
		[...LibraryQueryKeyFactory.all, 'artist-albums', artistId] as const,
	artistAppearances: (artistId: string) =>
		[...LibraryQueryKeyFactory.all, 'artist-appearances', artistId] as const,
	recentlyAdded: () => [...LibraryQueryKeyFactory.all, 'recently-added'] as const,
	stats: () => [...LibraryQueryKeyFactory.all, 'stats'] as const,
	scanSchedule: () => [...LibraryQueryKeyFactory.all, 'scan-schedule'] as const,
	albumSearch: (q: string) => [...LibraryQueryKeyFactory.all, 'album-search', q] as const,
	albumTracks: (mbid: string) => [...LibraryQueryKeyFactory.all, 'album-tracks', mbid] as const,
	search: (q: string) => [...LibraryQueryKeyFactory.all, 'search', q] as const,
	artistThumbs: () => [...LibraryQueryKeyFactory.all, 'artist-thumbs'] as const
};
