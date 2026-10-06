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
	items: ReleaseEditionResult[];
	total: number;
	offset: number;
	limit: number;
}

type ReleaseEditionSearchView = components['schemas']['ReleaseEditionSearchResponse'];

function toReleaseEditionSearch(view: ReleaseEditionSearchView): ReleaseEditionSearchResponse {
	return {
		...view,
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
	getEnabled: Getter<boolean> = () => true
) {
	return createQuery(() => {
		const userId = getUserId();
		const albumId = getAlbumId();
		const title = getTitle();
		const artist = getArtist();
		const offset = getOffset();
		return {
			enabled: getEnabled() && Boolean(albumId && title.trim()),
			queryKey: LibraryQueryKeyFactory.reidentificationReleases(
				userId,
				albumId,
				title,
				artist,
				offset
			),
			queryFn: async ({ signal }): Promise<ReleaseEditionSearchResponse> =>
				toReleaseEditionSearch(
					await api.global.v3.GET(
						LibraryV3Api.reidentificationReleases(albumId, { title, artist, limit: 12, offset }),
						{ signal }
					)
				)
		};
	});
}
