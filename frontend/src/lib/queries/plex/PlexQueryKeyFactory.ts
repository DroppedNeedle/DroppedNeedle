import { key, userScope } from '../keys';

export const PlexQueryKeyFactory = {
	libraries: (userId: string | null | undefined) => key(userScope(userId), 'plex', 'libraries')
};
