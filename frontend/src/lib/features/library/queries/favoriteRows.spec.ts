import { describe, expect, it } from 'vitest';

import { withFavorite } from './favoriteRows';

const album = { id: 'x', title: 'Low', is_compilation: false, favorite: false };
const track = { id: 'x', title: 'Warszawa', track_number: 4, favorite: false };

describe('withFavorite', () => {
	it('hearts only the row of the kind asked for, even when another kind shares its id', () => {
		const page = { album, tracks: [track] };

		const next = withFavorite(page, { kind: 'track', itemId: 'x', favorited: true });

		expect(next).toEqual({ album, tracks: [{ ...track, favorite: true }] });
		expect((next as typeof page).album).toBe(album);
	});

	it('hands back the same data when nothing changes, so no cache write follows', () => {
		const page = { items: [album] };

		expect(withFavorite(page, { kind: 'album', itemId: 'x', favorited: false })).toBe(page);
		expect(withFavorite(page, { kind: 'artist', itemId: 'x', favorited: true })).toBe(page);
	});
});
