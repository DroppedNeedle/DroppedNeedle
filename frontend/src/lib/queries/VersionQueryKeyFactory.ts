import { key, SHARED } from './keys';

export const VersionQueryKeyFactory = {
	info: () => key(SHARED, 'version', 'info'),
	updateCheck: () => key(SHARED, 'version', 'update-check'),
	releases: () => key(SHARED, 'version', 'releases')
};
