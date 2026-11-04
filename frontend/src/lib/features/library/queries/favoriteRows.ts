import type { AlbumView, ArtistView, TrackView } from './types';

export type FavoriteKind = 'album' | 'track' | 'artist';

interface FavoriteViews {
	album: AlbumView;
	artist: ArtistView;
	track: TrackView;
}

/** The item a heart changes, and its new state. */
export interface FavoriteChange {
	kind: FavoriteKind;
	itemId: string;
	favorited: boolean;
}

type Row = Record<string, unknown>;

// A field only each kind's view has, so an album never takes a track's
// heart when two ids match. The types hold each name to its view.
const KIND_FIELD: { [K in FavoriteKind]: keyof FavoriteViews[K] } = {
	album: 'is_compilation',
	artist: 'led_track_count',
	track: 'track_number'
};
const FAVORITE: keyof AlbumView & keyof ArtistView & keyof TrackView = 'favorite';

/** `data` with the item's flag set, or `data` itself when nothing in it changes. */
export function withFavorite(data: unknown, change: FavoriteChange): unknown {
	if (Array.isArray(data)) {
		const next = data.map((item) => withFavorite(item, change));
		return next.some((item, index) => item !== data[index]) ? next : data;
	}
	if (!data || typeof data !== 'object') return data;
	const row = data as Row;
	if (row.id === change.itemId && KIND_FIELD[change.kind] in row && FAVORITE in row) {
		return row[FAVORITE] === change.favorited ? data : { ...row, [FAVORITE]: change.favorited };
	}
	let changed = false;
	const next: Row = {};
	for (const [field, value] of Object.entries(row)) {
		next[field] = withFavorite(value, change);
		if (next[field] !== value) changed = true;
	}
	return changed ? next : data;
}
