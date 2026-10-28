import type { AlbumRef, ArtistRef } from '$lib/queries/discover/types';

export type TimeRangeItemType = 'album' | 'artist';

export function getTimeRangeFallbackPath(
	itemType: TimeRangeItemType,
	item: AlbumRef | ArtistRef
): string | null {
	if (itemType === 'album') {
		const album = item as AlbumRef;
		const query = [album.artist_name, album.title].filter(Boolean).join(' ').trim();
		return query ? `/search/albums?q=${encodeURIComponent(query)}` : null;
	}

	const query = 'name' in item ? item.name.trim() : '';
	return query ? `/search/artists?q=${encodeURIComponent(query)}` : null;
}
