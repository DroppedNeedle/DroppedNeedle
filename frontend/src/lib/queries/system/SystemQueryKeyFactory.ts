import { ADMIN, key, SHARED } from '../keys';

// Service health is the server's, the same for everyone.
export const SystemQueryKeyFactory = {
	health: () => key(SHARED, 'system', 'health'),
	/** The database backups and every check made of one. */
	backups: () => key(ADMIN, 'system', 'backups'),
	restoreReport: (name: string) => key(ADMIN, 'system', 'backups', name, 'restore-report')
};
