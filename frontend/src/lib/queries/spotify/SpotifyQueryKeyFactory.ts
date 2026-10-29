import { key, userScope } from '../keys';

type UserId = string | null | undefined;

export const SpotifyQueryKeyFactory = {
	playlists: (userId: UserId) => key(userScope(userId), 'spotify', 'playlists'),
	// The address follows the saved redirect origin, so a saved change reads it again.
	redirectUri: (userId: UserId, origin: string | null) =>
		key(userScope(userId), 'spotify', 'redirect-uri', origin)
};
