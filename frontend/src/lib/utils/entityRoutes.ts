import { withBasePath } from '$lib/utils/basePath';
import { isValidMbid } from '$lib/utils/formatting';

export function albumHref(id: string): string {
	return withBasePath(`/album/${encodeURIComponent(id)}`);
}

export function artistHref(id: string): string {
	return withBasePath(`/artist/${encodeURIComponent(id)}`);
}

/**
 * A library album's page: the release group it owns, else its own page. A
 * guess its files contradict owns no group, so it opens as itself.
 */
export function libraryAlbumHref(album: {
	id: string;
	release_group_mbid?: string | null;
	guess_contradicted: boolean;
}): string {
	return albumHref(
		album.release_group_mbid && !album.guess_contradicted ? album.release_group_mbid : album.id
	);
}

export function localAlbumHref(id: string): string {
	return albumHref(id);
}

export function localArtistHref(id: string): string {
	return artistHref(id);
}

export function albumHrefOrNull(id: string | null | undefined): string | null {
	return id ? albumHref(id) : null;
}

export function artistHrefOrNull(id: string | null | undefined): string | null {
	return id ? artistHref(id) : null;
}

/** The playing album's page: a MusicBrainz release group, or a local album on a local track. */
export function playedAlbumHref(albumId: string | null | undefined, local: boolean): string | null {
	return albumId && (isValidMbid(albumId) || local) ? albumHref(albumId) : null;
}

/** The playing track's credited artist page. */
export function playedArtistHref(artistId: string | null | undefined): string | null {
	return artistHrefOrNull(artistId);
}
