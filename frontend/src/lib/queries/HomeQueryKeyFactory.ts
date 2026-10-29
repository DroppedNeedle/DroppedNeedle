import { key, userScope } from './keys';

type UserId = string | null | undefined;

export const HomeQueryKeyFactory = {
	feature: 'home',
	home: (userId: UserId) => key(userScope(userId), 'home', 'page'),
	integrationStatus: (userId: UserId) => key(userScope(userId), 'home', 'integration-status')
};
