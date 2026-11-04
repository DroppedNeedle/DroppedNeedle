import type { components } from '$lib/api/v3/openapi';
import type {
	ArtistInfoBasic,
	ArtistReleases,
	ReleaseGroup
} from '$lib/features/album/queries/types';

type V3ArtistInfo = components['schemas']['ArtistInfo'];
type V3ArtistImages = components['schemas']['ArtistImages'];
type V3ArtistReleases = components['schemas']['ArtistReleases'];
type V3ReleaseItem = components['schemas']['ReleaseItem'];

const IMAGE_FIELDS = [
	'fanart_url',
	'fanart_url_2',
	'fanart_url_3',
	'fanart_url_4',
	'banner_url',
	'thumb_url',
	'wide_thumb_url',
	'logo_url',
	'clearart_url',
	'cutout_url'
] as const satisfies readonly (keyof V3ArtistImages)[];

/**
 * The artist page reads TheAudioDB images as flat fields. The header only
 * serves images already cached; the extended read fetches them, so its set
 * fills any field the header left empty.
 */
export function mergeArtistImages(
	primary: V3ArtistImages | null | undefined,
	fallback?: V3ArtistImages | null
): V3ArtistImages {
	const merged: V3ArtistImages = {};
	for (const field of IMAGE_FIELDS) {
		merged[field] = primary?.[field] ?? fallback?.[field] ?? null;
	}
	return merged;
}

/** The artist header in the page's flat shape. Releases come from their own paged read. */
export function toArtistInfoBasic(info: V3ArtistInfo): ArtistInfoBasic {
	const { albums: _albums, singles: _singles, eps: _eps, source: _source, images, ...rest } = info;
	return { ...rest, ...mergeArtistImages(images) };
}

function toReleaseGroup(item: V3ReleaseItem): ReleaseGroup {
	return { ...item, title: item.title ?? '' };
}

export function toArtistReleases(page: V3ArtistReleases): ArtistReleases {
	return {
		...page,
		albums: page.albums.map(toReleaseGroup),
		singles: page.singles.map(toReleaseGroup),
		eps: page.eps.map(toReleaseGroup)
	};
}
