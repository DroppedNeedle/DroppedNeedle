import { v3 } from '$lib/api/v3/endpoint';
import { createQuery } from '@tanstack/svelte-query';
import type { Getter } from 'runed';

import { api } from '$lib/api/client';
import { STALE_SLOW } from '$lib/queries/staleTime';
import { compareDiscTrack } from '$lib/player/queueHelpers';
import type { AlbumBasicInfo, YouTubeLink, YouTubeTrackLink } from '$lib/types';
import { AlbumQueryKeyFactory } from './AlbumQueryKeyFactory';
import { fetchAlbumTracks } from './albumTracks';
import { PERSIST, PERSIST_PROVIDER } from '$lib/queries/persistedCache';

type UserId = string | null | undefined;

// The album page's reads, keyed by the id the page was opened with (a
// release group, or a local album id the server resolves to one).

export const getAlbumBasicQuery = (getUserId: Getter<UserId>, getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			meta: PERSIST_PROVIDER,
			queryKey: AlbumQueryKeyFactory.basic(getUserId(), albumId),
			enabled: !!albumId,
			staleTime: STALE_SLOW,
			queryFn: ({ signal }: { signal: AbortSignal }): Promise<AlbumBasicInfo> =>
				api.v3.GET(v3('/api/v3/albums/{album_id}/basic', { path: { album_id: albumId } }), {
					signal
				})
		};
	});

export const getAlbumTracksQuery = (getUserId: Getter<UserId>, getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			meta: PERSIST_PROVIDER,
			queryKey: AlbumQueryKeyFactory.tracks(getUserId(), albumId),
			enabled: !!albumId,
			staleTime: STALE_SLOW,
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
			meta: PERSIST_PROVIDER,
			queryKey: AlbumQueryKeyFactory.moreByArtist(getUserId(), albumId, artistId),
			enabled: enabled && !!albumId && !!artistId,
			staleTime: STALE_SLOW,
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(
					v3('/api/v3/albums/{album_id}/more-by-artist', {
						path: { album_id: albumId },
						query: { artist_id: artistId }
					}),
					{ signal }
				)
		};
	});

export const getSimilarAlbumsQuery = (
	getUserId: Getter<UserId>,
	getParams: Getter<DiscoveryParams>
) =>
	createQuery(() => {
		const { albumId, artistId, enabled } = getParams();
		return {
			meta: PERSIST_PROVIDER,
			queryKey: AlbumQueryKeyFactory.similar(getUserId(), albumId, artistId),
			enabled: enabled && !!albumId && !!artistId,
			staleTime: STALE_SLOW,
			queryFn: ({ signal }: { signal: AbortSignal }) =>
				api.v3.GET(
					v3('/api/v3/albums/{album_id}/similar', {
						path: { album_id: albumId },
						query: { artist_id: artistId }
					}),
					{ signal }
				)
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
			meta: PERSIST,
			queryKey: AlbumQueryKeyFactory.lastFm(albumId, artistName, albumName),
			enabled: !!albumId,
			staleTime: STALE_SLOW,
			queryFn: async ({ signal }: { signal: AbortSignal }) =>
				(await api.v3.GET(
					v3('/api/v3/albums/{album_id}/lastfm', {
						path: { album_id: albumId },
						query: { artist_name: artistName, album_name: albumName }
					}),
					{
						signal
					}
				)) ?? null
		};
	});

// The album's saved YouTube link; none saved answers 204, read as null.
export const getAlbumYouTubeLinkQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			meta: PERSIST,
			queryKey: AlbumQueryKeyFactory.youtubeLink(albumId),
			enabled: !!albumId,
			staleTime: STALE_SLOW,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<YouTubeLink | null> =>
				(await api.v3.GET(v3('/api/v3/youtube/link/{album_id}', { path: { album_id: albumId } }), {
					signal
				})) ?? null
		};
	});

export const getAlbumYouTubeTrackLinksQuery = (getAlbumId: Getter<string>) =>
	createQuery(() => {
		const albumId = getAlbumId();
		return {
			meta: PERSIST,
			queryKey: AlbumQueryKeyFactory.youtubeTrackLinks(albumId),
			enabled: !!albumId,
			staleTime: STALE_SLOW,
			queryFn: async ({ signal }: { signal: AbortSignal }): Promise<YouTubeTrackLink[]> =>
				[
					...(await api.v3.GET(
						v3('/api/v3/youtube/track-links/{album_id}', { path: { album_id: albumId } }),
						{ signal }
					))
				].sort(compareDiscTrack)
		};
	});
