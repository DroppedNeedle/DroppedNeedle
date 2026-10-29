import { key, SHARED, userScope } from '../keys';

type UserId = string | null | undefined;

// The album page's reads. The header, the tracklist, the discovery shelves and
// the editions carry the owned and requested marks, so they are the caller's.
// Last.fm, the saved YouTube links, purchase links and plugin details are the
// same for everyone.
export const AlbumQueryKeyFactory = {
	feature: 'albums',
	basic: (userId: UserId, albumId: string) => key(userScope(userId), 'albums', 'basic', albumId),
	tracks: (userId: UserId, albumId: string) => key(userScope(userId), 'albums', 'tracks', albumId),
	moreByArtist: (userId: UserId, albumId: string, artistId: string) =>
		key(userScope(userId), 'albums', 'more-by-artist', albumId, artistId),
	similar: (userId: UserId, albumId: string, artistId: string) =>
		key(userScope(userId), 'albums', 'similar', albumId, artistId),
	editions: (userId: UserId, mbid: string) => key(userScope(userId), 'albums', 'editions', mbid),
	editionTracks: (userId: UserId, release: string) =>
		key(userScope(userId), 'albums', 'edition-tracks', release),
	lastFm: (albumId: string, artistName: string, albumName: string) =>
		key(SHARED, 'albums', 'lastfm', albumId, { artistName, albumName }),
	youtubeLink: (albumId: string) => key(SHARED, 'albums', 'youtube-link', albumId),
	youtubeTrackLinks: (albumId: string) => key(SHARED, 'albums', 'youtube-track-links', albumId),
	purchaseOptions: (mbid: string) => key(SHARED, 'albums', 'purchase-options', mbid),
	pluginDetails: (mbid: string) => key(SHARED, 'albums', 'plugin-details', mbid)
};
