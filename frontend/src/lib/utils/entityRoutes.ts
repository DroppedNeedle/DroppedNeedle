import { withBasePath } from '$lib/utils/basePath';

export function albumHref(id: string): string {
	return withBasePath(`/album/${encodeURIComponent(id)}`);
}

export function artistHref(id: string): string {
	return withBasePath(`/artist/${encodeURIComponent(id)}`);
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
