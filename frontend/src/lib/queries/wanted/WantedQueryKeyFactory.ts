import { key, userScope } from '../keys';

export const WantedQueryKeyFactory = {
	feature: 'wanted',
	list: (userId: string | null | undefined) => key(userScope(userId), 'wanted', 'list')
};
