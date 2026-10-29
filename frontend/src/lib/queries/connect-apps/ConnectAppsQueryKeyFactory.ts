import { ADMIN, key, SHARED, userScope } from '../keys';

export const ConnectAppsQueryKeyFactory = {
	feature: 'connect-apps',
	settings: () => key(SHARED, 'connect-apps', 'settings'),
	appPasswords: (userId: string) => key(userScope(userId), 'connect-apps', 'app-passwords'),
	// The oversight roster is the same for every admin.
	adminAppPasswords: () => key(ADMIN, 'connect-apps', 'app-passwords')
};
