import { userIdSegment } from '../userKeySegment';
import type {
	RemoteAlbumParams,
	RemoteArtistParams,
	RemoteLyricsParams,
	RemoteMixParams,
	RemotePageParams,
	RemoteRandomParams,
	RemoteTrackParams
} from './endpoints';
import type { RemoteSource } from './types';

type UserId = string | null | undefined;

// userId scopes every key (persisted cache on shared browsers). Keys nest
// source-first under `all`: one source-prefix sweep clears an adapter after
// a reconnect without touching the others.
export const RemoteQueryKeyFactory = {
	all: ['remotes'] as const,
	source: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.all, source, userIdSegment(userId)] as const,
	hub: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'hub'] as const,
	connection: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'connection'] as const,
	albums: (userId: UserId, source: RemoteSource, params: RemoteAlbumParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'albums',
			params.limit ?? null,
			params.offset ?? null,
			params.sort_by ?? '',
			params.sort_order ?? '',
			params.genre ?? '',
			params.year ?? null,
			params.decade ?? ''
		] as const,
	albumDetail: (userId: UserId, source: RemoteSource, id: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'albums', id] as const,
	albumTracks: (userId: UserId, source: RemoteSource, id: string, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'albums',
			id,
			'tracks',
			params.limit ?? null,
			params.offset ?? null
		] as const,
	artists: (userId: UserId, source: RemoteSource, params: RemoteArtistParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'artists',
			params.limit ?? null,
			params.offset ?? null,
			params.sort_by ?? '',
			params.sort_order ?? '',
			params.search ?? ''
		] as const,
	artistIndex: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'artists', 'index'] as const,
	artistDetail: (userId: UserId, source: RemoteSource, id: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'artists', id] as const,
	tracks: (userId: UserId, source: RemoteSource, params: RemoteTrackParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'tracks',
			params.limit ?? null,
			params.offset ?? null,
			params.sort_by ?? '',
			params.sort_order ?? '',
			params.search ?? '',
			params.genre ?? ''
		] as const,
	search: (userId: UserId, source: RemoteSource, q: string, limit: number | undefined) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'search', q, limit ?? null] as const,
	favorites: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'favorites'] as const,
	recent: (userId: UserId, source: RemoteSource, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'recent',
			params.limit ?? null,
			params.offset ?? null
		] as const,
	recentlyAdded: (userId: UserId, source: RemoteSource, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'recently-added',
			params.limit ?? null,
			params.offset ?? null
		] as const,
	history: (userId: UserId, source: RemoteSource, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'history',
			params.limit ?? null,
			params.offset ?? null
		] as const,
	stats: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'stats'] as const,
	sessions: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'sessions'] as const,
	genres: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'genres'] as const,
	genreSongs: (userId: UserId, source: RemoteSource, genre: string, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'genres',
			genre,
			params.limit ?? null,
			params.offset ?? null
		] as const,
	playlists: (userId: UserId, source: RemoteSource, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'playlists',
			params.limit ?? null,
			params.offset ?? null
		] as const,
	playlistDetail: (userId: UserId, source: RemoteSource, id: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'playlists', id] as const,
	infoAlbum: (userId: UserId, source: RemoteSource, id: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'info', 'albums', id] as const,
	infoArtist: (userId: UserId, source: RemoteSource, id: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'info', 'artists', id] as const,
	lyrics: (userId: UserId, source: RemoteSource, id: string, params: RemoteLyricsParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'lyrics',
			id,
			params.artist ?? '',
			params.title ?? ''
		] as const,
	match: (userId: UserId, source: RemoteSource, mbid: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'match', mbid] as const,
	mix: (userId: UserId, source: RemoteSource, id: string, params: RemoteMixParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'mix',
			id,
			params.kind ?? '',
			params.limit ?? null
		] as const,
	similar: (userId: UserId, source: RemoteSource, id: string, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'similar',
			id,
			params.limit ?? null,
			params.offset ?? null
		] as const,
	top: (userId: UserId, source: RemoteSource, artist: string, limit: number | undefined) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'top', artist, limit ?? null] as const,
	random: (userId: UserId, source: RemoteSource, params: RemoteRandomParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'random',
			params.limit ?? null,
			params.genre ?? ''
		] as const,
	discovery: (userId: UserId, source: RemoteSource, count: number | undefined) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'discovery', count ?? null] as const,
	folders: (userId: UserId) =>
		[...RemoteQueryKeyFactory.source(userId, 'navidrome'), 'folders'] as const
};
