import { ADMIN, key, userScope } from '../keys';

type UserId = string | null | undefined;

// The plugin list and files are admin state. Sources and plugin panels feed
// the caller's acquisition screens, so they are the caller's.
export const PluginQueryKeyFactory = {
	feature: 'plugins',
	list: () => key(ADMIN, 'plugins', 'list'),
	files: (name: string) => key(ADMIN, 'plugins', 'files', name),
	sources: (userId: UserId) => key(userScope(userId), 'plugins', 'sources'),
	ui: (userId: UserId, name: string) => key(userScope(userId), 'plugins', 'ui', name)
};
