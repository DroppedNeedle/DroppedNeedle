import { key, userScope } from '../keys';

export const ConnectionsQueryKeyFactory = {
	list: (userId: string | null | undefined) => key(userScope(userId), 'me', 'connections')
};
