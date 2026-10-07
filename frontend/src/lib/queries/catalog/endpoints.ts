import { v3 } from '$lib/api/v3/endpoint';
import type { MusicSource } from '$lib/stores/musicSource';

// The MusicBrainz-backed artist and album pages. Album ids are release-group
// MBIDs; the server also accepts a library album id and resolves it to the
// release group it was identified as.
export const CATALOG_ENDPOINTS = {
	artist: (mbid: string) => v3('/api/v3/artists/{artist_mbid}', { path: { artist_mbid: mbid } }),
	artistExtended: (mbid: string) =>
		v3('/api/v3/artists/{artist_mbid}/extended', { path: { artist_mbid: mbid } }),
	artistReleases: (mbid: string, offset: number, limit: number) =>
		v3('/api/v3/artists/{artist_mbid}/releases', {
			path: { artist_mbid: mbid },
			query: { offset, limit }
		}),
	similarArtists: (mbid: string, source: MusicSource, count = 15) =>
		v3('/api/v3/artists/{artist_mbid}/similar', {
			path: { artist_mbid: mbid },
			query: { count, source }
		}),
	topSongs: (mbid: string, source: MusicSource, count = 10) =>
		v3('/api/v3/artists/{artist_mbid}/top-songs', {
			path: { artist_mbid: mbid },
			query: { count, source }
		}),
	topAlbums: (mbid: string, source: MusicSource, count = 10) =>
		v3('/api/v3/artists/{artist_mbid}/top-albums', {
			path: { artist_mbid: mbid },
			query: { count, source }
		}),
	artistLastFm: (mbid: string, artistName: string) =>
		v3('/api/v3/artists/{artist_mbid}/lastfm', {
			path: { artist_mbid: mbid },
			query: { artist_name: artistName }
		}),
	artistPurchaseOptions: (mbid: string, artistName: string) =>
		v3('/api/v3/artists/{artist_mbid}/purchase-options', {
			path: { artist_mbid: mbid },
			query: { name: artistName }
		}),
	albumBasic: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/basic', { path: { album_id: albumId } }),
	albumTracks: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/tracks', { path: { album_id: albumId } }),
	albumRefresh: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/refresh', { path: { album_id: albumId } }),
	moreByArtist: (albumId: string, artistId: string) =>
		v3('/api/v3/albums/{album_id}/more-by-artist', {
			path: { album_id: albumId },
			query: { artist_id: artistId }
		}),
	similarAlbums: (albumId: string, artistId: string) =>
		v3('/api/v3/albums/{album_id}/similar', {
			path: { album_id: albumId },
			query: { artist_id: artistId }
		}),
	albumLastFm: (albumId: string, artistName: string, albumName: string) =>
		v3('/api/v3/albums/{album_id}/lastfm', {
			path: { album_id: albumId },
			query: { artist_name: artistName, album_name: albumName }
		}),
	albumPurchaseOptions: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/purchase-options', { path: { album_id: albumId } }),
	editions: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/editions', { path: { album_id: albumId } }),
	editionTracks: (albumId: string, releaseMbid: string) =>
		v3('/api/v3/albums/{album_id}/editions/{release_mbid}/tracks', {
			path: { album_id: albumId, release_mbid: releaseMbid }
		}),
	// Choose an edition for the group's one library copy (404 with no copy,
	// 409 when several copies match the group).
	editionPin: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/edition', { path: { album_id: albumId } }),
	acquireEdition: (albumId: string) =>
		v3('/api/v3/albums/{album_id}/edition/acquire', { path: { album_id: albumId } })
} as const;
