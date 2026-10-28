import type { AlbumRef, ArtistRef, ChartSection, GenreRef } from '$lib/queries/discover/types';

/** The album rows of a shelf, in order. */
export function albumRows(section: ChartSection | null | undefined): AlbumRef[] {
	return (section?.items ?? []).flatMap((item) => (item.kind === 'album' ? [item] : []));
}

/** The artist rows of a shelf, in order. */
export function artistRows(section: ChartSection | null | undefined): ArtistRef[] {
	return (section?.items ?? []).flatMap((item) => (item.kind === 'artist' ? [item] : []));
}

/** The genre rows of a shelf, in order. */
export function genreRows(section: ChartSection | null | undefined): GenreRef[] {
	return (section?.items ?? []).flatMap((item) => (item.kind === 'genre' ? [item] : []));
}
