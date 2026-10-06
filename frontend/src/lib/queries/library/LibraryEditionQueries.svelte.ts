import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

type Getter<T> = () => T;

export interface ReleaseEditionResult {
	release_mbid: string;
	release_group_mbid: string;
	artist_name: string;
	title: string;
	date: string | null;
	country: string | null;
	status: string | null;
	packaging: string | null;
	media_formats: string[];
	disc_count: number;
	track_count: number;
	label: string | null;
	catalogue_number: string | null;
	barcode: string | null;
	disambiguation: string | null;
	musicbrainz_url: string;
	score: number;
	belongs_to_current_release_group: boolean;
	is_current_release: boolean;
}

export interface ReleaseEditionSearchResponse {
	title_query: string;
	artist_query: string;
	/** Set when the page lists every release of one release group. */
	release_group_query: string | null;
	current_release_group_mbid: string | null;
	items: ReleaseEditionResult[];
	total: number;
	offset: number;
	limit: number;
}

type ReleaseEditionSearchView = components['schemas']['ReleaseEditionSearchResponse'];

function toReleaseEditionSearch(view: ReleaseEditionSearchView): ReleaseEditionSearchResponse {
	return {
		...view,
		release_group_query: view.release_group_query ?? null,
		current_release_group_mbid: view.current_release_group_mbid ?? null,
		items: view.items.map((item) => ({
			...item,
			date: item.date ?? null,
			country: item.country ?? null,
			status: item.status ?? null,
			packaging: item.packaging ?? null,
			label: item.label ?? null,
			catalogue_number: item.catalogue_number ?? null,
			barcode: item.barcode ?? null,
			disambiguation: item.disambiguation ?? null
		}))
	};
}

export function getReleaseEditionSearchQuery(
	getUserId: Getter<string | undefined>,
	getAlbumId: Getter<string>,
	getTitle: Getter<string>,
	getArtist: Getter<string>,
	getOffset: Getter<number>,
	getEnabled: Getter<boolean> = () => true,
	/** List every release of this release group instead of searching. */
	getReleaseGroup: Getter<string | null> = () => null
) {
	return createQuery(() => {
		const userId = getUserId();
		const albumId = getAlbumId();
		const title = getTitle();
		const artist = getArtist();
		const offset = getOffset();
		const releaseGroup = getReleaseGroup();
		return {
			enabled: getEnabled() && Boolean(albumId && (releaseGroup || title.trim())),
			queryKey: LibraryQueryKeyFactory.reidentificationReleases(
				userId,
				albumId,
				releaseGroup ? `group:${releaseGroup}` : title,
				releaseGroup ? '' : artist,
				offset
			),
			queryFn: async ({ signal }): Promise<ReleaseEditionSearchResponse> =>
				toReleaseEditionSearch(
					await api.global.v3.GET(
						LibraryV3Api.reidentificationReleases(
							albumId,
							releaseGroup
								? { title: '', artist: '', release_group_mbid: releaseGroup, limit: 12, offset }
								: { title, artist, limit: 12, offset }
						),
						{ signal }
					)
				)
		};
	});
}
