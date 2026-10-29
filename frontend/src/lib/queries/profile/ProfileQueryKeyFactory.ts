import { key, userScope } from '../keys';

export const ProfileQueryKeyFactory = {
	profile: (userId: string) => key(userScope(userId), 'profile')
};
