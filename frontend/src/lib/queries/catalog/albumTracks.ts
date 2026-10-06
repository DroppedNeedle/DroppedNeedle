import { api } from '$lib/api/client';
import type { AlbumTracksInfo } from '$lib/types';
import { CATALOG_ENDPOINTS } from './endpoints';

/** The tracklist of the release the album page serves for a group. */
export function fetchAlbumTracks(albumId: string, signal?: AbortSignal): Promise<AlbumTracksInfo> {
	return api.global.v3.GET(CATALOG_ENDPOINTS.albumTracks(albumId), { signal });
}
