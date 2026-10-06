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
import { API } from '$lib/constants';
import { compareDiscTrack } from '$lib/player/queueHelpers';
import {
	fetchJellyfinAlbumMatch,
	fetchLocalAlbumMatch,
	fetchNavidromeAlbumMatch,
	fetchPlexAlbumMatch
} from '$lib/queries/albumMatch';

export { fetchAlbumTracks } from '$lib/api/albums';

export async function fetchAlbumBasic(
	albumId: string,
	signal?: AbortSignal
): Promise<AlbumBasicInfo> {
	return api.get<AlbumBasicInfo>(`/api/v1/albums/${albumId}/basic`, { signal });
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
		api
			.get<MoreByArtistResponse>(`/api/v1/albums/${albumId}/more-by-artist?artist_id=${artistId}`, {
				signal
			})
			.catch(() => null),
		api
			.get<SimilarAlbumsResponse>(`/api/v1/albums/${albumId}/similar?artist_id=${artistId}`, {
				signal
			})
			.catch(() => null)
	]);
	return { moreByArtist, similarAlbums };
}

export async function fetchYouTubeAlbumLink(
	albumId: string,
	signal?: AbortSignal
): Promise<YouTubeLink | null> {
	return api.get<YouTubeLink>(API.youtube.link(albumId), { signal }).catch(() => null);
}

export async function fetchYouTubeTrackLinks(
	albumId: string,
	signal?: AbortSignal
): Promise<YouTubeTrackLink[]> {
	const data = await api
		.get<YouTubeTrackLink[]>(API.youtube.trackLinks(albumId), { signal })
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
	const params = new URLSearchParams({
		artist_name: opts.artistName,
		album_name: opts.albumName
	});
	return api.get<LastFmAlbumEnrichment>(`/api/v1/albums/${albumId}/lastfm?${params.toString()}`, {
		signal
	});
}

export async function refreshAlbum(
	albumId: string,
	signal?: AbortSignal
): Promise<AlbumBasicInfo | null> {
	return api
		.post<AlbumBasicInfo>(`/api/v1/albums/${albumId}/refresh`, undefined, { signal })
		.catch(() => null);
}
