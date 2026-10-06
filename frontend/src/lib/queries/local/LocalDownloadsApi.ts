import { v3 } from '$lib/api/v3/endpoint';

// /api/v3 local file download URLs, built through the typed registry. The
// download buttons across the album, card and source views import these and
// hand them to the blob downloader (utils/downloadActions).
export const LocalDownloadsApi = {
	access: () => v3('/api/v3/download/access'),
	track: (trackId: string | number) =>
		v3('/api/v3/download/local/track/{id}', { path: { id: trackId } }),
	album: (albumId: string) => v3('/api/v3/download/local/album/{id}', { path: { id: albumId } }),
	albumByMbid: (mbid: string) => v3('/api/v3/download/local/album/mbid/{mbid}', { path: { mbid } })
};
