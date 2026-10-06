import { userIdSegment } from '../userKeySegment';
import type { RemoteMixParams, RemotePageParams, RemoteRandomParams } from './endpoints';
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
	artistIndex: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'artists', 'index'] as const,
	favorites: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'favorites'] as const,
	history: (userId: UserId, source: RemoteSource, params: RemotePageParams) =>
		[
			...RemoteQueryKeyFactory.source(userId, source),
			'history',
			params.limit ?? null,
			params.offset ?? null
		] as const,
	stats: (userId: UserId, source: RemoteSource) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'stats'] as const,
	infoArtist: (userId: UserId, source: RemoteSource, id: string) =>
		[...RemoteQueryKeyFactory.source(userId, source), 'info', 'artists', id] as const,
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
