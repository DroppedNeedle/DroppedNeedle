import { userIdSegment } from '$lib/queries/userKeySegment';

// Jobs are user-dependent (each curator sees their own; admins can see all), so the
// key carries the userId segment - the IndexedDB-persisted cache must never show one
// user's import history to another on a shared browser.
export const DropImportQueryKeyFactory = {
	prefix: ['drop-import'] as const,
	jobs: (userId: string | null | undefined, all: boolean) =>
		[...DropImportQueryKeyFactory.prefix, 'jobs', userIdSegment(userId), { all }] as const
};
