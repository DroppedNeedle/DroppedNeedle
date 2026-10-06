import type {
	AlbumBasicInfo,
	MoreByArtistResponse,
	SimilarAlbumsResponse,
	YouTubeLink,
	YouTubeTrackLink,
	JellyfinAlbumMatch,
	LocalAlbumMatch,
	NavidromeAlbumMatch,
	PlexAlbumMatch,
	LastFmAlbumEnrichment
} from '$lib/types';
import { api } from '$lib/api/client';
import { YOUTUBE_ENDPOINTS } from '$lib/queries/youtube/endpoints';
import { CATALOG_ENDPOINTS } from '$lib/queries/catalog/endpoints';
import { compareDiscTrack } from '$lib/player/queueHelpers';
import {
	fetchJellyfinAlbumMatch,
	fetchLocalAlbumMatch,
	fetchNavidromeAlbumMatch,
	fetchPlexAlbumMatch
} from '$lib/queries/albumMatch';

export { fetchAlbumTracks } from '$lib/queries/catalog/albumTracks';

export async function fetchAlbumBasic(
	albumId: string,
	signal?: AbortSignal
): Promise<AlbumBasicInfo> {
	return api.v3.GET(CATALOG_ENDPOINTS.albumBasic(albumId), { signal });
}

export async function fetchDiscovery(
	albumId: string,
	artistId: string,
	signal?: AbortSignal
): Promise<{
	moreByArtist: MoreByArtistResponse | null;
	similarAlbums: SimilarAlbumsResponse | null;
}> {
	const [moreByArtist, similarAlbums] = await Promise.all([
		api.v3.GET(CATALOG_ENDPOINTS.moreByArtist(albumId, artistId), { signal }).catch(() => null),
		api.v3.GET(CATALOG_ENDPOINTS.similarAlbums(albumId, artistId), { signal }).catch(() => null)
	]);
	return { moreByArtist, similarAlbums };
}

export async function fetchYouTubeAlbumLink(
	albumId: string,
	signal?: AbortSignal
): Promise<YouTubeLink | null> {
	// 204 (no link saved) arrives as undefined.
	return api.v3
		.GET(YOUTUBE_ENDPOINTS.link(albumId), { signal })
		.then((link) => link ?? null)
		.catch(() => null);
}

export async function fetchYouTubeTrackLinks(
	albumId: string,
	signal?: AbortSignal
): Promise<YouTubeTrackLink[]> {
	const data = await api.v3
		.GET(YOUTUBE_ENDPOINTS.trackLinks(albumId), { signal })
		.catch(() => null);
	return data ? data.sort(compareDiscTrack) : [];
}

export async function fetchJellyfinMatch(
	albumId: string,
	signal?: AbortSignal
): Promise<JellyfinAlbumMatch | null> {
	return fetchJellyfinAlbumMatch(albumId, signal);
}

export async function fetchLocalMatch(
	albumId: string,
	signal?: AbortSignal
): Promise<LocalAlbumMatch | null> {
	return fetchLocalAlbumMatch(albumId, signal);
}

export async function fetchNavidromeMatch(
	albumId: string,
	_opts: { albumTitle?: string; artistName?: string },
	signal?: AbortSignal
): Promise<NavidromeAlbumMatch | null> {
	// v3 matches by MusicBrainz id only; the title and artist hints are unused.
	return fetchNavidromeAlbumMatch(albumId, signal);
}

export async function fetchPlexMatch(
	albumId: string,
	_opts: { albumTitle?: string; artistName?: string },
	signal?: AbortSignal
): Promise<PlexAlbumMatch | null> {
	return fetchPlexAlbumMatch(albumId, signal);
}

export async function fetchLastFm(
	albumId: string,
	opts: { artistName: string; albumName: string },
	signal?: AbortSignal
): Promise<LastFmAlbumEnrichment | null> {
	return api.v3.GET(CATALOG_ENDPOINTS.albumLastFm(albumId, opts.artistName, opts.albumName), {
		signal
	});
}

export async function refreshAlbum(
	albumId: string,
	signal?: AbortSignal
): Promise<AlbumBasicInfo | null> {
	return api.v3
		.POST(CATALOG_ENDPOINTS.albumRefresh(albumId), undefined, { signal })
		.catch(() => null);
}
