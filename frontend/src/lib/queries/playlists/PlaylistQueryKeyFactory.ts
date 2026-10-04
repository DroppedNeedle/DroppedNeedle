import { userIdSegment } from '../userKeySegment';

export type PlaylistV3UserId = string | null | undefined;

export const PlaylistQueryKeyFactory = {
	prefix: ['playlists'] as const,
	// Keys carry the current user id so personalized playlist data never leaks across
	// a user switch on a shared browser (AMU-5).
	list: (userId: string | undefined) =>
		[...PlaylistQueryKeyFactory.prefix, userId ?? 'anon', 'list'] as const,
	detail: (userId: string | undefined, id: string) =>
		[...PlaylistQueryKeyFactory.prefix, userId ?? 'anon', 'detail', id] as const,
	// v3 playlist keys, nested under the same prefix for prefix invalidation.
	v3: {
		root: (userId: PlaylistV3UserId) =>
			[...PlaylistQueryKeyFactory.prefix, 'v3', userIdSegment(userId)] as const,
		list: (userId: PlaylistV3UserId) =>
			[...PlaylistQueryKeyFactory.v3.root(userId), 'list'] as const,
		detail: (userId: PlaylistV3UserId, id: string) =>
			[...PlaylistQueryKeyFactory.v3.root(userId), 'detail', id] as const
	}
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
