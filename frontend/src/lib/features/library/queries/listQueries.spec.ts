import { describe, expect, it } from 'vitest';
import { albumsQuery, artistsQuery, tracksQuery } from './listQueries';
import { libraryTrackParams } from './libraryTracks';

describe('list queries', () => {
	it('send the track bound in either direction and the caller stars', () => {
		expect(
			albumsQuery(
				{ limit: 50, minTracks: 3, tracksBelow: true, rating: 4, ratingMode: 'at_most' },
				null
			)
		).toEqual({
			limit: 50,
			min_tracks: 3,
			tracks_mode: 'below',
			rating: 4,
			rating_mode: 'at_most'
		});
		expect(
			artistsQuery({ limit: 48, scope: 'contributors', minTracks: 2, rating: 5 }, 'next')
		).toEqual({ limit: 48, after: 'next', scope: 'contributors', min_tracks: 2, rating: 5 });
	});

	it('leave out a bound that keeps everything', () => {
		expect(albumsQuery({ limit: 50, minTracks: 1, tracksBelow: true, rating: 0 }, null)).toEqual({
			limit: 50
		});
	});

	it('filter the track list by format and stars', () => {
		const params = libraryTrackParams(48, 'recent', '', false, {
			formats: ['flac', 'mp3'],
			formatMode: 'without',
			rating: 3
		});
		expect(tracksQuery(params, null)).toEqual({
			limit: 48,
			sort: 'date_added',
			order: 'desc',
			formats: 'flac,mp3',
			format_mode: 'without',
			rating: 3
		});
	});
});
