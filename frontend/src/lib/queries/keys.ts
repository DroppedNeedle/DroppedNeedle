import type { QueryKey } from '@tanstack/svelte-query';

/**
 * The one query key builder. Every key is `[scope, feature, ...parts]`:
 *
 * - the scope says whose data it is: one user's (`user:<id>`), the signed-out
 *   visitor's (`signed-out`, which no user scope can equal), everyone's
 *   (`shared`), or the admins' (`admin`);
 * - the feature names the area, so one filter finds a feature in every scope.
 *
 * Everything cached for one user is therefore the one prefix
 * `[userScope(id)]`. A key holds only what changes the answer: whether a query
 * persists or depends on the MusicBrainz source is its `meta`, not its key.
 */
export const SIGNED_OUT = 'signed-out';
export const SHARED = 'shared';
export const ADMIN = 'admin';

export type UserScope = `user:${string}` | typeof SIGNED_OUT;
export type KeyScope = UserScope | typeof SHARED | typeof ADMIN;

/** The scope of data that belongs to the caller. */
export function userScope(userId: string | null | undefined): UserScope {
	return userId ? `user:${userId}` : SIGNED_OUT;
}

export function key<const F extends string, const P extends readonly unknown[]>(
	scope: KeyScope,
	feature: F,
	...parts: P
): readonly [KeyScope, F, ...P] {
	return [scope, feature, ...parts];
}

/**
 * A filter for every cached query under `[feature, ...parts]`, whatever its
 * scope: what a change to shared data refreshes in the user and admin views
 * alike.
 */
export function everyScope(feature: string, ...parts: string[]) {
	const tail = [feature, ...parts];
	return {
		predicate: (query: { queryKey: QueryKey }) =>
			tail.every((part, i) => query.queryKey[i + 1] === part)
	};
}
