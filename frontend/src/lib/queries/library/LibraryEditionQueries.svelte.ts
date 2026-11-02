import { createQuery } from '@tanstack/svelte-query';

import { api } from '$lib/api/client';
import type { components } from '$lib/api/v3/openapi';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';
import { LibraryV3Api } from './LibraryV3Api';

type Getter<T> = () => T;

export type ReleaseEditionSearchResponse = components['schemas']['ReleaseEditionSearchResponse'];
export type ReleaseEditionResult = ReleaseEditionSearchResponse['items'][number];

export function getReleaseEditionSearchQuery(
	getUserId: Getter<string | undefined>,
	getAlbumId: Getter<string>,
	getTitle: Getter<string>,
	getArtist: Getter<string>,
	getEnabled: Getter<boolean> = () => true,
	/** List every release of this release group instead of searching. */
	getReleaseGroup: Getter<string | null> = () => null
) {
	return createQuery(() => {
		const userId = getUserId();
		const albumId = getAlbumId();
		const title = getTitle();
		const artist = getArtist();
		const releaseGroup = getReleaseGroup();
		return {
			enabled: getEnabled() && Boolean(albumId && (releaseGroup || title.trim())),
			queryKey: LibraryQueryKeyFactory.reidentificationReleases(
				userId,
				albumId,
				releaseGroup ? `group:${releaseGroup}` : title,
				releaseGroup ? '' : artist
			),
			queryFn: ({ signal }): Promise<ReleaseEditionSearchResponse> =>
				api.v3.GET(
					LibraryV3Api.reidentificationReleases(
						albumId,
						releaseGroup
							? { title: '', artist: '', release_group_mbid: releaseGroup, limit: 12 }
							: { title, artist, limit: 12 }
					),
					{ signal }
				)
		};
	});
}
