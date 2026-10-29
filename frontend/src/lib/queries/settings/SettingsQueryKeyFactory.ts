import { key, userScope } from '../keys';

type UserId = string | null | undefined;

// Each settings form reads the section as the signed-in admin saw it.
export const SettingsQueryKeyFactory = {
	feature: 'settings',
	section: (userId: UserId, name: string) => key(userScope(userId), 'settings', name),
	jellyfin: (userId: UserId) => key(userScope(userId), 'settings', 'jellyfin'),
	lyrics: (userId: UserId) => key(userScope(userId), 'settings', 'lyrics'),
	publicAddress: (userId: UserId) => key(userScope(userId), 'settings', 'public-address'),
	cacheStats: (userId: UserId) => key(userScope(userId), 'admin', 'cache', 'stats')
};
