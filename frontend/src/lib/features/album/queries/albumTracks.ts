import { v3 } from '$lib/api/v3/endpoint';
import { api } from '$lib/api/client';
import type { AlbumTracksInfo } from '$lib/features/album/queries/types';

/** The tracklist of the release the album page serves for a group. */
export function fetchAlbumTracks(albumId: string, signal?: AbortSignal): Promise<AlbumTracksInfo> {
	return api.v3.GET(v3('/api/v3/albums/{album_id}/tracks', { path: { album_id: albumId } }), {
		signal
	});
}
