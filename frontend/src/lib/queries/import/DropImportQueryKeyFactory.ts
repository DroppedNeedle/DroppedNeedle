// Jobs are user-dependent (each curator sees their own; admins can see all), so the
// key carries the userId segment - the IndexedDB-persisted cache must never show one
// user's import history to another on a shared browser.
export const DropImportQueryKeyFactory = {
	prefix: ['drop-import'] as const,
	jobs: (userId: string | undefined, all: boolean) =>
		[...DropImportQueryKeyFactory.prefix, 'jobs', userId ?? 'anon', { all }] as const
};
