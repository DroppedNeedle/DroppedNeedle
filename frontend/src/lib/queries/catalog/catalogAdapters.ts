import type { components } from '$lib/api/v3/openapi';
import type { ArtistImageFields, ArtistInfoBasic, ArtistReleases, ReleaseGroup } from '$lib/types';

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
] as const satisfies readonly (keyof V3ArtistImages & keyof ArtistImageFields)[];

/**
 * The artist page reads TheAudioDB images as flat fields. The header only
 * serves images already cached; the extended read fetches them, so its set
 * fills any field the header left empty.
 */
export function mergeArtistImages(
	primary: V3ArtistImages | ArtistImageFields | null | undefined,
	fallback?: V3ArtistImages | null
): ArtistImageFields {
	const merged: ArtistImageFields = {};
	for (const field of IMAGE_FIELDS) {
		merged[field] = primary?.[field] ?? fallback?.[field] ?? null;
	}
	return merged;
}

/** The artist header in the page's flat shape. Releases come from their own paged read. */
export function toArtistInfoBasic(info: V3ArtistInfo): ArtistInfoBasic {
	return {
		name: info.name,
		musicbrainz_id: info.musicbrainz_id,
		disambiguation: info.disambiguation,
		type: info.type,
		country: info.country,
		life_span: info.life_span,
		...mergeArtistImages(info.images),
		tags: info.tags,
		aliases: info.aliases,
		external_links: info.external_links,
		in_library: info.in_library,
		appears_in_library: info.appears_in_library,
		followed: info.followed,
		auto_download: info.auto_download,
		auto_download_state: info.auto_download_state as ArtistInfoBasic['auto_download_state'],
		release_group_count: info.release_group_count,
		service_status: info.service_status
	};
}

function toReleaseGroup(item: V3ReleaseItem): ReleaseGroup {
	return {
		id: item.id,
		title: item.title ?? '',
		type: item.type ?? undefined,
		year: item.year ?? undefined,
		first_release_date: item.first_release_date ?? undefined,
		in_library: item.in_library,
		requested: item.requested
	};
}

export function toArtistReleases(page: V3ArtistReleases): ArtistReleases {
	return {
		albums: page.albums.map(toReleaseGroup),
		singles: page.singles.map(toReleaseGroup),
		eps: page.eps.map(toReleaseGroup),
		offset: page.offset,
		limit: page.limit,
		returned_count: page.returned_count,
		next_offset: page.next_offset ?? null,
		has_more: page.has_more,
		source_total_count: page.source_total_count,
		warming: page.warming,
		service_status: page.service_status
	};
}
