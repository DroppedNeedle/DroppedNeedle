import type { MusicSource } from '$lib/stores/musicSource';
import { key, SHARED, userScope } from '../keys';

type UserId = string | null | undefined;

// The artist page. The header, releases, links and image credit are the same
// for everyone; the top lists and similar artists follow the caller's
// listening source, so they are the caller's.
export const ArtistQueryKeyFactory = {
	feature: 'artist',
	basic: (id: string) => key(SHARED, 'artist', id),
	extended: (id: string) => key(SHARED, 'artist', id, 'extended'),
	releases: (id: string) => key(SHARED, 'artist', id, 'releases'),
	allReleases: (id: string) => key(SHARED, 'artist', id, 'releases', 'all'),
	lastFmEnrichment: (id: string, artistName?: string) =>
		key(SHARED, 'artist', id, 'lastfm-enrichment', { artistName }),
	imageCredit: (mbid: string) => key(SHARED, 'artist', 'image-credit', mbid),
	purchaseOptions: (mbid: string) => key(SHARED, 'artist', 'purchase-options', mbid),
	topAlbums: (userId: UserId, id: string, source: MusicSource) =>
		key(userScope(userId), 'artist', id, 'top-albums', { source }),
	topSongs: (userId: UserId, id: string, source: MusicSource) =>
		key(userScope(userId), 'artist', id, 'top-songs', { source }),
	similarArtists: (userId: UserId, id: string, source: MusicSource) =>
		key(userScope(userId), 'artist', id, 'similar-artists', { source })
};
