import { describe, expect, it } from 'vitest';
import {
	toSearchRemoteStatus,
	toSuggestResultsV1,
	toV1Album,
	toV1Artist,
	type SearchResultItemV3,
	type SuggestResultV3
} from './SearchV3Adapters';

const artistRow = (overrides: Partial<SearchResultItemV3> = {}): SearchResultItemV3 => ({
	kind: 'artist',
	id: 'local-artist-1',
	title: 'Muse',
	musicbrainz_id: 'mbid-artist-1',
	in_library: true,
	requested: false,
	score: 100,
	...overrides
});

const albumRow = (overrides: Partial<SearchResultItemV3> = {}): SearchResultItemV3 => ({
	kind: 'album',
	id: 'local-album-1',
	title: 'Absolution',
	artist: 'Muse',
	year: 2003,
	musicbrainz_id: 'mbid-album-1',
	in_library: true,
	requested: false,
	score: 90,
	...overrides
});

describe('toV1Artist', () => {
	it('maps an identified row onto the shared card shape', () => {
		expect.assertions(1);
		expect(toV1Artist(artistRow())).toEqual({
			title: 'Muse',
			musicbrainz_id: 'mbid-artist-1',
			in_library: true,
			score: 100,
			local_id: 'local-artist-1'
		});
	});

	it('falls back to the local id when the row has no provider id', () => {
		expect.assertions(2);
		const artist = toV1Artist(artistRow({ musicbrainz_id: null }));
		expect(artist.musicbrainz_id).toBe('local-artist-1');
		expect(artist.local_id).toBe('local-artist-1');
	});
});

describe('toV1Album', () => {
	it('maps an identified row onto the shared card shape', () => {
		expect.assertions(1);
		expect(toV1Album(albumRow())).toEqual({
			title: 'Absolution',
			artist: 'Muse',
			year: 2003,
			musicbrainz_id: 'mbid-album-1',
			in_library: true,
			requested: false,
			score: 90,
			local_id: 'local-album-1'
		});
	});

	it('falls back to the local id when the row has no provider id', () => {
		expect.assertions(3);
		const album = toV1Album(albumRow({ musicbrainz_id: null, artist: null, year: null }));
		expect(album.musicbrainz_id).toBe('local-album-1');
		expect(album.artist).toBeNull();
		expect(album.year).toBeNull();
	});
});

describe('toSuggestResultsV1', () => {
	const suggestRow = (overrides: Partial<SuggestResultV3> = {}): SuggestResultV3 => ({
		kind: 'artist',
		id: 'local-artist-1',
		title: 'Muse',
		musicbrainz_id: 'mbid-artist-1',
		score: 100,
		...overrides
	});

	it('maps artist and album rows onto the typeahead shape as library hits', () => {
		expect.assertions(1);
		expect(
			toSuggestResultsV1([
				suggestRow(),
				suggestRow({
					kind: 'album',
					id: 'local-album-1',
					title: 'Absolution',
					artist: 'Muse',
					musicbrainz_id: 'mbid-album-1',
					score: 90
				})
			])
		).toEqual([
			{
				type: 'artist',
				title: 'Muse',
				artist: null,
				musicbrainz_id: 'mbid-artist-1',
				in_library: true,
				requested: false,
				score: 100,
				local_id: 'local-artist-1'
			},
			{
				type: 'album',
				title: 'Absolution',
				artist: 'Muse',
				musicbrainz_id: 'mbid-album-1',
				in_library: true,
				requested: false,
				score: 90,
				local_id: 'local-album-1'
			}
		]);
	});

	it('drops track rows and falls back to the local id without a provider id', () => {
		expect.assertions(2);
		const rows = toSuggestResultsV1([
			suggestRow({ kind: 'track', id: 'local-track-1', title: 'Hysteria' }),
			suggestRow({ musicbrainz_id: null })
		]);
		expect(rows).toHaveLength(1);
		expect(rows[0]).toMatchObject({
			type: 'artist',
			musicbrainz_id: 'local-artist-1',
			local_id: 'local-artist-1'
		});
	});
});

describe('toSearchRemoteStatus', () => {
	it('passes every contract status through to the v1 helpers', () => {
		expect.assertions(5);
		for (const status of ['ok', 'partial', 'timeout', 'error', 'stale'] as const) {
			expect(toSearchRemoteStatus(status)).toBe(status);
		}
	});
});
