import { describe, expect, it } from 'vitest';
import { LibraryQueryKeyFactory } from './LibraryQueryKeyFactory';

const albumsParams = { limit: 50, offset: 0, sort: 'name', order: 'asc' } as const;

describe('LibraryQueryKeyFactory.v3', () => {
	it('roots user-scoped v3 keys under the library prefix', () => {
		expect(LibraryQueryKeyFactory.v3.root('user-a')).toEqual(['library', 'v3', 'user-a']);
	});

	it('normalizes a missing userId to null', () => {
		expect(LibraryQueryKeyFactory.v3.root(undefined)).toEqual(['library', 'v3', null]);
	});

	it('differs per user (no cross-user collision on favorite-bearing views)', () => {
		expect(LibraryQueryKeyFactory.v3.albums('user-a', { ...albumsParams })).not.toEqual(
			LibraryQueryKeyFactory.v3.albums('user-b', { ...albumsParams })
		);
	});

	it('nests every v3 key under the versioned user root for prefix invalidation', () => {
		const userId = 'user-a';
		const v3 = LibraryQueryKeyFactory.v3;
		const keys = [
			v3.albums(userId, { ...albumsParams }),
			v3.artists(userId, { limit: 48, offset: 0, sort: 'name', order: 'asc' }),
			v3.artistThumbs(userId),
			v3.albumDetail(userId, 'album-1'),
			v3.albumCopies(userId, 'album-1'),
			v3.albumTracks(userId, 'album-1', {}),
			v3.artistDetail(userId, 'artist-1'),
			v3.artistAlbums(userId, 'artist-1', {}),
			v3.artistAppearances(userId, 'artist-1', {}),
			v3.tracks(userId, { limit: 50, offset: 0, sort: 'title', order: 'asc' }),
			v3.trackDetail(userId, 'track-1'),
			v3.lyrics(userId, 'track-1'),
			v3.genres(userId),
			v3.genreTracks(userId, 'Rock', {}),
			v3.recentlyAdded(userId, 20),
			v3.stats(userId),
			v3.reviews(userId, 'album-1'),
			v3.editionPin(userId, 'album-1'),
			v3.scanRuns(userId),
			v3.scanRun(userId, 'run-1'),
			v3.roots(userId)
		];
		expect(keys.length).toBeGreaterThan(0);
		for (const key of keys) {
			expect([...key].slice(0, 3)).toEqual(['library', 'v3', userId]);
		}
	});
});
