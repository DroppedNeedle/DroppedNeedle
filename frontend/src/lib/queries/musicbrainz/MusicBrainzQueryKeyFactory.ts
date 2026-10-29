import { key, userScope } from '../keys';

export const MusicBrainzQueryKeyFactory = {
	settings: (userId: string | null | undefined) => key(userScope(userId), 'musicbrainz', 'settings')
};
