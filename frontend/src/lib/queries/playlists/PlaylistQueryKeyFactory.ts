import { userIdSegment } from '../userKeySegment';

export type PlaylistV3UserId = string | null | undefined;

// The one key factory for the caller's playlists. Every key sits under the
// user's root, so a mutation or event that changes playlists invalidates
// `root(userId)` and the list and every open detail refresh together. The
// user id keeps one account's playlists from showing for another on a
// shared browser.
export const PlaylistQueryKeyFactory = {
	prefix: ['playlists'] as const,
	root: (userId: PlaylistV3UserId) =>
		[...PlaylistQueryKeyFactory.prefix, userIdSegment(userId)] as const,
	list: (userId: PlaylistV3UserId) => [...PlaylistQueryKeyFactory.root(userId), 'list'] as const,
	detail: (userId: PlaylistV3UserId, id: string) =>
		[...PlaylistQueryKeyFactory.root(userId), 'detail', id] as const
};

export type FavoriteV3UserId = string | null | undefined;

export type FavoriteV3Kind = 'album' | 'artist' | 'track';

// Favorite flags ride on library views, so invalidation for this root pairs
// with the library v3 root (see the favorites mutation briefs).
export const FavoriteQueryKeyFactory = {
	prefix: ['favorites'] as const,
	user: (userId: FavoriteV3UserId) =>
		[...FavoriteQueryKeyFactory.prefix, userIdSegment(userId)] as const,
	list: (userId: FavoriteV3UserId, kind: FavoriteV3Kind | null) =>
		[...FavoriteQueryKeyFactory.user(userId), 'list', kind] as const
};
