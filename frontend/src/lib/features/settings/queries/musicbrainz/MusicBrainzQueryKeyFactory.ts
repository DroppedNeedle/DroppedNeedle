import { key, userScope } from '$lib/queries/keys';

export const MusicBrainzQueryKeyFactory = {
	settings: (userId: string | null | undefined) => key(userScope(userId), 'musicbrainz', 'settings')
};
