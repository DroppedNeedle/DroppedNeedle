import type { ArtistReleases, ReleaseGroup } from '$lib/types';
import { dedupeById } from '$lib/utils/dedupe';

export interface MergedArtistReleases {
	albums: ReleaseGroup[];
	eps: ReleaseGroup[];
	singles: ReleaseGroup[];
}

function newestFirst(releases: ReleaseGroup[]): ReleaseGroup[] {
	return [...releases].sort((a, b) => {
		if (a.year === null || a.year === undefined) return 1;
		if (b.year === null || b.year === undefined) return -1;
		return b.year - a.year;
	});
}

export function mergeArtistReleasePages(pages: ArtistReleases[]): MergedArtistReleases {
	return {
		albums: newestFirst(dedupeById(pages.flatMap((page) => page.albums))),
		eps: newestFirst(dedupeById(pages.flatMap((page) => page.eps))),
		singles: newestFirst(dedupeById(pages.flatMap((page) => page.singles)))
	};
}
