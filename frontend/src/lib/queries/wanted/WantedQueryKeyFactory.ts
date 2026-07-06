// userId scopes the key: the cache persists to IndexedDB across refresh on
// shared browsers, and a keyless list would leak one user's watches to another.
export const WantedQueryKeyFactory = {
	prefix: ['wanted'] as const,
	list: (userId: string | undefined) =>
		[...WantedQueryKeyFactory.prefix, 'list', userId ?? 'anon'] as const
};
