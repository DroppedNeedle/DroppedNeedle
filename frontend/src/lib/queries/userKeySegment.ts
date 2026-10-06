/**
 * The userId segment convention for user-dependent query keys: a missing
 * userId normalizes to null so logged-out keys stay stable and never collide
 * with a real user id. Use this in every key factory that takes a userId.
 */
export function userIdSegment(userId: string | null | undefined): string | null {
	return userId ?? null;
}

/**
 * The same convention for string keys (browser storage, cache namespaces):
 * a missing userId is spelled `null`, never a made-up word like 'anon'.
 */
export function userStorageSegment(userId: string | null | undefined): string {
	return userIdSegment(userId) ?? 'null';
}
