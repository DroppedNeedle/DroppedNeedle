import { describe, expect, it } from 'vitest';

import { ArtistQueryKeyFactory } from './ArtistQueryKeyFactory';

describe('artist discovery query keys', () => {
	it('scopes similar artists by authenticated user without changing artist or source dimensions', () => {
		const userA = ArtistQueryKeyFactory.similarArtists('user-a', 'artist-1', 'listenbrainz');
		const userB = ArtistQueryKeyFactory.similarArtists('user-b', 'artist-1', 'listenbrainz');

		expect(userA).toEqual([
			'artist',
			'user-a',
			{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 },
			'artist-1',
			'similar-artists',
			{ source: 'listenbrainz' }
		]);
		expect(userB).not.toEqual(userA);
	});
	it('scopes top songs and top albums by authenticated user without changing dimensions', () => {
		const topSongsA = ArtistQueryKeyFactory.topSongs('user-a', 'artist-1', 'listenbrainz');
		const topAlbumsA = ArtistQueryKeyFactory.topAlbums('user-a', 'artist-1', 'lastfm');

		expect(topSongsA).toEqual([
			'artist',
			'user-a',
			{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 },
			'artist-1',
			'top-songs',
			{ source: 'listenbrainz' }
		]);
		expect(topAlbumsA).toEqual([
			'artist',
			'user-a',
			{ user_id: 'user-a', source_mode: 'brainzmash', source_id: '', generation: 0 },
			'artist-1',
			'top-albums',
			{ source: 'lastfm' }
		]);
		expect(ArtistQueryKeyFactory.topSongs('user-b', 'artist-1', 'listenbrainz')).not.toEqual(
			topSongsA
		);
		expect(ArtistQueryKeyFactory.topAlbums('user-b', 'artist-1', 'lastfm')).not.toEqual(topAlbumsA);
	});
});
