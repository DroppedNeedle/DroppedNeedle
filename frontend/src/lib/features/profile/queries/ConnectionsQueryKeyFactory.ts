import { key, userScope } from '$lib/queries/keys';

export const ConnectionsQueryKeyFactory = {
	list: (userId: string | null | undefined) => key(userScope(userId), 'me', 'connections')
};
