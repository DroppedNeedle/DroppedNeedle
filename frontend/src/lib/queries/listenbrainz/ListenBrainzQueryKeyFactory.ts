import { ADMIN, key } from '../keys';

// The instance's ListenBrainz connection, an admin setting.
export const ListenBrainzQueryKeyFactory = {
	connection: () => key(ADMIN, 'settings', 'listenbrainz')
};
