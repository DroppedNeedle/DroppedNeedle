import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { CACHE_TTL } from '$lib/constants';
import { compareDiscTrack } from '$lib/player/queueHelpers';
import { YOUTUBE_ENDPOINTS } from '$lib/queries/youtube/endpoints';
import type { AlbumBasicInfo, YouTubeLink, YouTubeTrackLink } from '$lib/types';
import { extractServiceStatus } from '$lib/utils/serviceStatus';
import { AlbumQueryKeyFactory } from './AlbumQueryKeyFactory';
import { fetchAlbumTracks } from './albumTracks';
import { CATALOG_ENDPOINTS } from './endpoints';

type UserId = string | null | undefined;

// The album page's reads, keyed by the id the page was opened with (a
// release group, or a local album id the server resolves to one).

export const getAlbumBasicQuery = (getUserId: Getter<UserId>, getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			queryKey: AlbumQueryKeyFactory.basic(getUserId(), albumId),
			enabled: !!albumId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_BASIC,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<AlbumBasicInfo> => {
				const album = await api.global.v3.GET(CATALOG_ENDPOINTS.albumBasic(albumId), { signal });
				extractServiceStatus(album);
				return album;
			}
		};
	});

export const getAlbumTracksQuery = (getUserId: Getter<UserId>, getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			queryKey: AlbumQueryKeyFactory.tracks(getUserId(), albumId),
			enabled: !!albumId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_TRACKS,
			queryFn: ({ signal }: { signal: AbortSignal }) => fetchAlbumTracks(albumId, signal)
		};
	});

interface DiscoveryParams {
	albumId: string;
	artistId: string;
	/** The shelves wait for the tracklist, which the page needs first. */
	enabled: boolean;
}

export const getMoreByArtistQuery = (
	getUserId: Getter<UserId>,
	getParams: Getter<DiscoveryParams>
) =>
	createQuery(() => {
		const { albumId, artistId, enabled } = getParams();
		return {
			queryKey: AlbumQueryKeyFactory.moreByArtist(getUserId(), albumId, artistId),
			enabled: enabled && !!albumId && !!artistId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_DISCOVERY,
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.global.v3.GET(CATALOG_ENDPOINTS.moreByArtist(albumId, artistId), { signal })
		};
	});

export const getSimilarAlbumsQuery = (
	getUserId: Getter<UserId>,
	getParams: Getter<DiscoveryParams>
) =>
	createQuery(() => {
		const { albumId, artistId, enabled } = getParams();
		return {
			queryKey: AlbumQueryKeyFactory.similar(getUserId(), albumId, artistId),
			enabled: enabled && !!albumId && !!artistId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_DISCOVERY,
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.global.v3.GET(CATALOG_ENDPOINTS.similarAlbums(albumId, artistId), { signal })
		};
	});

// Last.fm answers null when it is not configured.
export const getAlbumLastFmQuery = (
	getParams: Getter<{ albumId: string; artistName: string; albumName: string } | null>
) =>
	createQuery(() => {
		const params = getParams();
		const albumId = params?.albumId ?? '';
		const artistName = params?.artistName ?? '';
		const albumName = params?.albumName ?? '';
		return {
			queryKey: AlbumQueryKeyFactory.lastFm(albumId, artistName, albumName),
			enabled: !!albumId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_LASTFM,
			queryFn: async ({ signal }: { signal: AbortSignal }) =>
				(await api.global.v3.GET(CATALOG_ENDPOINTS.albumLastFm(albumId, artistName, albumName), {
					signal
				})) ?? null
		};
	});

// The album's saved YouTube link; none saved answers 204, read as null.
export const getAlbumYouTubeLinkQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			queryKey: AlbumQueryKeyFactory.youtubeLink(albumId),
			enabled: !!albumId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_YOUTUBE,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<YouTubeLink | null> =>
				(await api.global.v3.GET(YOUTUBE_ENDPOINTS.link(albumId), { signal })) ?? null
		};
	});

export const getAlbumYouTubeTrackLinksQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			queryKey: AlbumQueryKeyFactory.youtubeTrackLinks(albumId),
			enabled: !!albumId,
			staleTime: CACHE_TTL.ALBUM_DETAIL_YOUTUBE,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<YouTubeTrackLink[]> =>
				[...(await api.global.v3.GET(YOUTUBE_ENDPOINTS.trackLinks(albumId), { signal }))].sort(
					compareDiscTrack
				)
		};
	});
