import { key, userScope } from '../keys';

type UserId = string | null | undefined;

// An artist's MusicBrainz link. The caller's: it says whether they set it.
// Under the library feature, so a catalog refresh reloads it.
export const ArtistLinkQueryKeyFactory = {
	link: (userId: UserId, artistId: string) =>
		key(userScope(userId), 'library', 'artist-link', artistId)
};
