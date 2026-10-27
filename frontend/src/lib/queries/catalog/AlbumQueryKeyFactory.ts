import { musicBrainzSourceKey } from '../musicbrainz/sourceScope.svelte';
import { userIdSegment } from '../userKeySegment';

type UserId = string | null | undefined;

// The album page's catalog reads. The header, the tracklist and the discovery
// shelves carry the caller's owned and requested marks, so their keys hold the
// user id; they come from MusicBrainz, so they also hold the source scope a
// source switch sweeps. Last.fm and the saved YouTube links are the same for
// everyone.
const providerKey = (segment: string, userId: UserId) => {
	const user = userIdSegment(userId);
	return [...AlbumQueryKeyFactory.prefix, segment, user, musicBrainzSourceKey(user)] as const;
};

export const AlbumQueryKeyFactory = {
	prefix: ['albums'] as const,
	basic: (userId: UserId, albumId: string) => [...providerKey('basic', userId), albumId] as const,
	tracks: (userId: UserId, albumId: string) => [...providerKey('tracks', userId), albumId] as const,
	moreByArtist: (userId: UserId, albumId: string, artistId: string) =>
		[...providerKey('more-by-artist', userId), albumId, artistId] as const,
	similar: (userId: UserId, albumId: string, artistId: string) =>
		[...providerKey('similar', userId), albumId, artistId] as const,
	lastFm: (albumId: string, artistName: string, albumName: string) =>
		[...AlbumQueryKeyFactory.prefix, 'lastfm', albumId, { artistName, albumName }] as const,
	youtubeLink: (albumId: string) =>
		[...AlbumQueryKeyFactory.prefix, 'youtube-link', albumId] as const,
	youtubeTrackLinks: (albumId: string) =>
		[...AlbumQueryKeyFactory.prefix, 'youtube-track-links', albumId] as const
};
