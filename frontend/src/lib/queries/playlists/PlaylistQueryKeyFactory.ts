import { key, userScope } from '../keys';

export type PlaylistV3UserId = string | null | undefined;

// The one key factory for the caller's playlists. Every key sits under the
// user's root, so a mutation or event that changes playlists invalidates
// `root(userId)` and the list and every open detail refresh together.
export const PlaylistQueryKeyFactory = {
	feature: 'playlists',
	root: (userId: PlaylistV3UserId) => key(userScope(userId), 'playlists'),
	list: (userId: PlaylistV3UserId) => [...PlaylistQueryKeyFactory.root(userId), 'list'] as const,
	detail: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'detail', id] as const,
	sources: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'sources', id] as const
};
