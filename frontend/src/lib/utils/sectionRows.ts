import type { AlbumRef, ChartSection, GenreRef } from '$lib/queries/discover/types';

/** The album rows of a shelf, in order. */
export function albumRows(section: ChartSection | null | undefined): AlbumRef[] {
	return (section?.items ?? []).flatMap((item) => (item.kind === 'album' ? [item] : []));
}

/** The genre rows of a shelf, in order. */
export function genreRows(section: ChartSection | null | undefined): GenreRef[] {
	return (section?.items ?? []).flatMap((item) => (item.kind === 'genre' ? [item] : []));
}
