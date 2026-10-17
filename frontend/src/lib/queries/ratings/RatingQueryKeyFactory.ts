import { userIdSegment } from '../userKeySegment';

export type RatingUserId = string | null | undefined;

export type RatingKind = 'album' | 'track' | 'artist';

/** Sorted and de-duplicated, so the same ids in any order share a key. */
function keyIds(ids: string[]): string[] {
	return [...new Set(ids)].sort();
}

// The caller's own star ratings. Every key sits under the user's root, so
// one invalidation refreshes every page's lookup.
export const RatingQueryKeyFactory = {
	prefix: ['ratings'] as const,
	user: (userId: RatingUserId) => [...RatingQueryKeyFactory.prefix, userIdSegment(userId)] as const,
	lookup: (userId: RatingUserId, albumIds: string[], trackIds: string[], artistIds: string[]) =>
		[
			...RatingQueryKeyFactory.user(userId),
			'lookup',
			keyIds(albumIds),
			keyIds(trackIds),
			keyIds(artistIds)
		] as const
};
