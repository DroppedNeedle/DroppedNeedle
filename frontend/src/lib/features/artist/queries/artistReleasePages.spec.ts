import { describe, expect, it } from 'vitest';

import type { ArtistReleases, ReleaseGroup } from '$lib/types';

import { mergeArtistReleasePages } from './artistReleasePages';

function release(id: string, year?: number): ReleaseGroup {
	return { id, title: id, type: 'Album', year, in_library: false, requested: false };
}

function page(
	offset: number,
	groups: Partial<Pick<ArtistReleases, 'albums' | 'eps' | 'singles'>>
): ArtistReleases {
	return {
		albums: groups.albums ?? [],
		eps: groups.eps ?? [],
		singles: groups.singles ?? [],
		offset,
		limit: 50,
		returned_count: 0,
		next_offset: offset + 100,
		has_more: true,
		source_total_count: 300,
		service_status: null,
		warming: false
	};
}

describe('artist release page merging', () => {
	it('preserves grouping, deduplicates page overlap, and sorts each group newest first', () => {
		const merged = mergeArtistReleasePages([
			page(0, {
				albums: [release('album-old', 2001), release('album-new', 2024)],
				eps: [release('ep-undated')]
			}),
			page(100, {
				albums: [release('album-old', 2001), release('album-mid', 2018)],
				eps: [release('ep-new', 2020)],
				singles: [release('single', 2019)]
			})
		]);

		expect(merged.albums.map((item) => item.id)).toEqual(['album-new', 'album-mid', 'album-old']);
		expect(merged.eps.map((item) => item.id)).toEqual(['ep-new', 'ep-undated']);
		expect(merged.singles.map((item) => item.id)).toEqual(['single']);
	});
});
