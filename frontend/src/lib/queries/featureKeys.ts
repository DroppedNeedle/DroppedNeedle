import type { SourceType } from '$lib/player/types';
import { ADMIN, key, SHARED, userScope } from '$lib/queries/keys';
import type { RemoteMatchHints } from '$lib/queries/remotes/params';
import type { RemoteSource } from '$lib/queries/remotes/types';
import type { SourcePlaylistSource } from '$lib/types';

// The keys of the small features that effects.ts sweeps. They live apart from
// those features' one file so that effects.ts, which every mutation imports,
// does not import every hook in turn.
type UserId = string | null | undefined;

// An artist's MusicBrainz link. The caller's: it says whether they set it.
// Under the library feature, so a catalog refresh reloads it.
export const ArtistLinkQueryKeyFactory = {
	link: (userId: UserId, artistId: string) =>
		key(userScope(userId), 'library', 'artist-link', artistId)
};

export const GenreQueryKeyFactory = {
	feature: 'genre',
	detail: (userId: UserId, genre: string) =>
		key(userScope(userId), 'genre', genre.trim().toLocaleLowerCase()),
	artistPages: (userId: UserId, genre: string) =>
		[...GenreQueryKeyFactory.detail(userId, genre), 'artists'] as const,
	albumPages: (userId: UserId, genre: string) =>
		[...GenreQueryKeyFactory.detail(userId, genre), 'albums'] as const
};

export const HomeQueryKeyFactory = {
	feature: 'home',
	home: (userId: UserId) => key(userScope(userId), 'home', 'page'),
	integrationStatus: (userId: UserId) => key(userScope(userId), 'home', 'integration-status')
};

// Each curator sees their own import jobs (admins may see all), so the jobs
// are the caller's.
export const DropImportQueryKeyFactory = {
	feature: 'drop-import',
	jobs: (userId: string | null | undefined, all: boolean) =>
		key(userScope(userId), 'drop-import', 'jobs', { all })
};

export const LyricsQueryKeyFactory = {
	feature: 'lyrics',
	lyrics: (
		userId: string | null | undefined,
		navidromeScope: string | undefined,
		sourceType: SourceType | undefined,
		trackSourceId: string | undefined,
		artistName: string | undefined,
		trackName: string | undefined
	) =>
		key(
			userScope(userId),
			'lyrics',
			navidromeScope,
			sourceType,
			trackSourceId,
			artistName,
			trackName
		)
};

type RatingUserId = string | null | undefined;

/** Sorted and de-duplicated, so the same ids in any order share a key. */
function keyIds(ids: string[]): string[] {
	return [...new Set(ids)].sort();
}

// The caller's own star ratings. Every key sits under the user's root, so
// one invalidation refreshes every page's lookup.
export const RatingQueryKeyFactory = {
	feature: 'ratings',
	user: (userId: RatingUserId) => key(userScope(userId), 'ratings'),
	lookup: (userId: RatingUserId, albumIds: string[], trackIds: string[], artistIds: string[]) =>
		[
			...RatingQueryKeyFactory.user(userId),
			'lookup',
			keyIds(albumIds),
			keyIds(trackIds),
			keyIds(artistIds)
		] as const
};

export const WantedQueryKeyFactory = {
	feature: 'wanted',
	list: (userId: string | null | undefined) => key(userScope(userId), 'wanted', 'list')
};

// Keyed by user (the remote sources are the caller's own connections), then
// album, so one album's sources or all of a user's refresh together.
export const AlbumMatchKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'album-match'),
	album: (userId: UserId, mbid: string) => [...AlbumMatchKeyFactory.user(userId), mbid] as const,
	local: (userId: UserId, mbid: string) =>
		[...AlbumMatchKeyFactory.album(userId, mbid), 'local'] as const,
	remote: (userId: UserId, source: RemoteSource, mbid: string, hints: RemoteMatchHints) =>
		[
			...AlbumMatchKeyFactory.album(userId, mbid),
			source,
			source === 'jellyfin' ? '' : (hints.title ?? ''),
			source === 'jellyfin' ? '' : (hints.artist ?? '')
		] as const
};

export const ConnectAppsQueryKeyFactory = {
	feature: 'connect-apps',
	settings: () => key(SHARED, 'connect-apps', 'settings'),
	appPasswords: (userId: string) => key(userScope(userId), 'connect-apps', 'app-passwords'),
	mine: (userId: string) => key(userScope(userId), 'connect-apps', 'mine'),
	// The oversight roster is the same for every admin.
	adminAppPasswords: () => key(ADMIN, 'connect-apps', 'app-passwords'),
	// The apps that may sign in through the browser.
	appConnect: () => key(ADMIN, 'connect-apps', 'app-connect'),
	// What an app asks of the caller at /connect.
	appConnectRequest: (userId: string | null | undefined, request: object | null) =>
		key(userScope(userId), 'connect-apps', 'app-connect-request', request)
};

// The connection is the one admin setting. The candidates say which artists
// the caller already follows, so they are the caller's.
export const LidarrImportQueryKeyFactory = {
	feature: 'lidarr-import',
	config: () => key(ADMIN, 'lidarr-import', 'config'),
	candidates: (userId: string | null | undefined) =>
		key(userScope(userId), 'lidarr-import', 'candidates')
};

export const ProfileQueryKeyFactory = {
	profile: (userId: string) => key(userScope(userId), 'profile')
};

export const ScrobblePreferencesQueryKeyFactory = {
	get: (userId: string | null | undefined) => key(userScope(userId), 'me', 'scrobble-preferences'),
	// The admin queue, the same for every admin.
	personalMixApprovals: () => key(ADMIN, 'me', 'scrobble-preferences', 'personal-mix-approvals')
};

// Under the auth feature, beside the other sign-in data.
export const SessionsQueryKeyFactory = {
	list: (userId: string | null | undefined) => key(userScope(userId), 'auth', 'sessions', 'list')
};

export const SourcePlaylistQueryKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'source-playlists'),
	source: (userId: UserId, source: SourcePlaylistSource) =>
		[...SourcePlaylistQueryKeyFactory.user(userId), source] as const,
	list: (userId: UserId, source: SourcePlaylistSource, limit: number) =>
		[...SourcePlaylistQueryKeyFactory.source(userId, source), 'list', limit] as const,
	detail: (userId: UserId, source: SourcePlaylistSource, playlistId: string) =>
		[...SourcePlaylistQueryKeyFactory.source(userId, source), 'detail', playlistId] as const
};

export const SpotifyQueryKeyFactory = {
	playlists: (userId: UserId) => key(userScope(userId), 'spotify', 'playlists'),
	// The address follows the saved redirect origin, so a saved change reads it again.
	redirectUri: (userId: UserId, origin: string | null) =>
		key(userScope(userId), 'spotify', 'redirect-uri', origin)
};

// Service health is the server's, the same for everyone.
export const SystemQueryKeyFactory = {
	health: () => key(SHARED, 'system', 'health'),
	/** The database backups and every check made of one. */
	backups: () => key(ADMIN, 'system', 'backups'),
	restoreReport: (name: string) => key(ADMIN, 'system', 'backups', name, 'restore-report')
};
