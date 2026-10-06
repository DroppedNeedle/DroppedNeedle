import { userIdSegment } from '$lib/queries/userKeySegment';
import type { SourcePlaylistSource } from '$lib/types';

export const SourcePlaylistQueryKeyFactory = {
	prefix: ['source-playlists'] as const,
	user: (userId: string | null | undefined) =>
		[...SourcePlaylistQueryKeyFactory.prefix, userIdSegment(userId)] as const,
	source: (userId: string | null | undefined, source: SourcePlaylistSource) =>
		[...SourcePlaylistQueryKeyFactory.user(userId), source] as const,
	list: (userId: string | null | undefined, source: SourcePlaylistSource, limit: number) =>
		[...SourcePlaylistQueryKeyFactory.source(userId, source), 'list', limit] as const,
	detail: (userId: string | null | undefined, source: SourcePlaylistSource, playlistId: string) =>
		[...SourcePlaylistQueryKeyFactory.source(userId, source), 'detail', playlistId] as const
};
