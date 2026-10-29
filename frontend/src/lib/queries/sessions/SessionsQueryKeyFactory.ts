import { key, userScope } from '../keys';

// Under the auth feature, beside the other sign-in data.
export const SessionsQueryKeyFactory = {
	list: (userId: string | null | undefined) => key(userScope(userId), 'auth', 'sessions', 'list')
};
