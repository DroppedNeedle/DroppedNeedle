import type { AlbumTracksInfo } from '$lib/types';
import { api } from '$lib/api/client';
import { API } from '$lib/constants';

export async function fetchAlbumTracks(
	albumId: string,
	signal?: AbortSignal
): Promise<AlbumTracksInfo> {
	return api.global.get<AlbumTracksInfo>(API.album.tracks(albumId), { signal });
}
