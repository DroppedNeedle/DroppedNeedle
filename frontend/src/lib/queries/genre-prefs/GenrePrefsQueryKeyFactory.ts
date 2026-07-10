export const GenrePrefsQueryKeyFactory = {
	prefix: ['genre-prefs'] as const,
	// userId dimension: prefs are per-user and the cache persists across refreshes
	// on shared browsers.
	prefs: (userId: string | null | undefined) =>
		[...GenrePrefsQueryKeyFactory.prefix, userId ?? null] as const
};
