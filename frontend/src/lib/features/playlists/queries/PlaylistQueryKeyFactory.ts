import { key, userScope } from '$lib/queries/keys';

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
		[...PlaylistQueryKeyFactory.root(userId), 'sources', id] as const,
	members: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'members', id] as const,
	smartList: (userId: PlaylistV3UserId) =>
		[...PlaylistQueryKeyFactory.root(userId), 'smart-list'] as const,
	smartDetail: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'smart-detail', id] as const,
	smartMatches: (userId: PlaylistV3UserId) =>
		[...PlaylistQueryKeyFactory.root(userId), 'smart-matches'] as const,
	smartTracks: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.smartMatches(userId), id] as const,
	smartTracksPage: (userId: PlaylistV3UserId, id: string, after: string | null) =>
		[...PlaylistQueryKeyFactory.smartTracks(userId, id), after] as const,
	people: (userId: PlaylistV3UserId) =>
		[...PlaylistQueryKeyFactory.root(userId), 'people'] as const,
	requestState: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'request-state', id] as const,
	sync: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'sync', id] as const
};
