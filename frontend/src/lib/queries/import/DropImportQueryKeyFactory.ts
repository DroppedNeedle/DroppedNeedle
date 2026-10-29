import { key, userScope } from '../keys';

// Each curator sees their own import jobs (admins may see all), so the jobs
// are the caller's.
export const DropImportQueryKeyFactory = {
	feature: 'drop-import',
	jobs: (userId: string | null | undefined, all: boolean) =>
		key(userScope(userId), 'drop-import', 'jobs', { all })
};
