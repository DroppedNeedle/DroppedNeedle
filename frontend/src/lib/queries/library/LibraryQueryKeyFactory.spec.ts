import { describe, expect, it } from 'vitest';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

const albumsParams = { limit: 50, offset: 0, sort: 'name', order: 'asc' } as const;

describe('LibraryQueryKeyFactory.catalog', () => {
	it('roots user-scoped catalog keys under the library prefix', () => {
		expect(LibraryQueryKeyFactory.catalog.root('user-a')).toEqual(['library', 'catalog', 'user-a']);
	});

	it('differs per user (no cross-user collision on favorite-bearing views)', () => {
		expect(LibraryQueryKeyFactory.catalog.albums('user-a', { ...albumsParams })).not.toEqual(
			LibraryQueryKeyFactory.catalog.albums('user-b', { ...albumsParams })
		);
	});

	it('nests every catalog key under the user root for prefix invalidation', () => {
		const userId = 'user-a';
		const catalog = LibraryQueryKeyFactory.catalog;
		const keys = [
			catalog.albums(userId, { ...albumsParams }),
			catalog.artists(userId, { limit: 48, offset: 0, sort: 'name', order: 'asc' }),
			catalog.artistThumbs(userId),
			catalog.albumDetail(userId, 'album-1'),
			catalog.albumCopies(userId, 'album-1'),
			catalog.albumTracks(userId, 'album-1', {}),
			catalog.artistDetail(userId, 'artist-1'),
			catalog.artistAlbums(userId, 'artist-1', {}),
			catalog.artistAppearances(userId, 'artist-1', {}),
			catalog.recentlyAdded(userId, 20),
			catalog.search(userId, 'abba'),
			catalog.albumSearch(userId, 'abba'),
			catalog.stats(userId),
			catalog.editionPin(userId, 'album-1')
		];
		expect(keys.length).toBeGreaterThan(0);
		for (const key of keys) {
			expect([...key].slice(0, 3)).toEqual(['library', 'catalog', userId]);
		}
	});

	it('keeps logged-out keys apart from every user', () => {
		expect(LibraryQueryKeyFactory.catalog.root(undefined)).toEqual(['library', 'catalog', null]);
		expect(LibraryQueryKeyFactory.membership(undefined, ['a'])).toEqual([
			'library',
			'membership',
			null,
			['a']
		]);
		expect(LibraryQueryKeyFactory.activity('user-a')).not.toEqual(
			LibraryQueryKeyFactory.activity('user-b')
		);
	});
});
