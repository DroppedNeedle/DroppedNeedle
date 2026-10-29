import type { SourcePlaylistSource } from '$lib/types';
import { key, userScope } from '../keys';

type UserId = string | null | undefined;

export const SourcePlaylistQueryKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'source-playlists'),
	source: (userId: UserId, source: SourcePlaylistSource) =>
		[...SourcePlaylistQueryKeyFactory.user(userId), source] as const,
	list: (userId: UserId, source: SourcePlaylistSource, limit: number) =>
		[...SourcePlaylistQueryKeyFactory.source(userId, source), 'list', limit] as const,
	detail: (userId: UserId, source: SourcePlaylistSource, playlistId: string) =>
		[...SourcePlaylistQueryKeyFactory.source(userId, source), 'detail', playlistId] as const
};
