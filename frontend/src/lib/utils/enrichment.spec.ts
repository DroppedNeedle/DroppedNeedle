import { describe, it, expect } from 'vitest';
import { getListenTitle, applyArtistEnrichment, applyAlbumEnrichment } from './enrichment';
import type { EnrichmentResponseV3 } from '../queries/search/SearchV3Queries.svelte';
import type { Artist, Album } from '$lib/types';

describe('getListenTitle', () => {
	it('returns "Last.fm listeners" for lastfm artist', () => {
		expect.assertions(1);
		expect(getListenTitle('lastfm', 'artist')).toBe('Last.fm listeners');
	});

	it('returns "Last.fm plays" for lastfm album', () => {
		expect.assertions(1);
		expect(getListenTitle('lastfm', 'album')).toBe('Last.fm plays');
	});

	it('returns "ListenBrainz plays" for listenbrainz artist', () => {
		expect.assertions(1);
		expect(getListenTitle('listenbrainz', 'artist')).toBe('ListenBrainz plays');
	});

	it('returns "ListenBrainz plays" for listenbrainz album', () => {
		expect.assertions(1);
		expect(getListenTitle('listenbrainz', 'album')).toBe('ListenBrainz plays');
	});

	it('returns "Plays" for none source', () => {
		expect.assertions(1);
		expect(getListenTitle('none')).toBe('Plays');
	});

	it('defaults kind to artist', () => {
		expect.assertions(1);
		expect(getListenTitle('lastfm')).toBe('Last.fm listeners');
	});
});

describe('applyArtistEnrichment', () => {
	const baseArtist: Artist = {
		musicbrainz_id: 'art-1',
		title: 'Muse',
		in_library: false,
		release_group_count: null,
		listen_count: null
	};

	it('applies enrichment data to matching artists', () => {
		expect.assertions(2);
		const enrichment: EnrichmentResponseV3 = {
			artists: [{ musicbrainz_id: 'art-1', release_group_count: 10, listen_count: 5000 }],
			albums: [],
			degradations: [],
			source: 'listenbrainz'
		};
		const result = applyArtistEnrichment([baseArtist], enrichment);
		expect(result[0].release_group_count).toBe(10);
		expect(result[0].listen_count).toBe(5000);
	});

	it('preserves zero listen_count from enrichment', () => {
		expect.assertions(1);
		const enrichment: EnrichmentResponseV3 = {
			artists: [{ musicbrainz_id: 'art-1', release_group_count: 3, listen_count: 0 }],
			albums: [],
			degradations: [],
			source: 'lastfm'
		};
		const result = applyArtistEnrichment([{ ...baseArtist, listen_count: 999 }], enrichment);
		expect(result[0].listen_count).toBe(0);
	});

	it('keeps existing value when enrichment listen_count is null', () => {
		expect.assertions(1);
		const enrichment: EnrichmentResponseV3 = {
			artists: [{ musicbrainz_id: 'art-1', release_group_count: null, listen_count: null }],
			albums: [],
			degradations: [],
			source: 'none'
		};
		const result = applyArtistEnrichment([{ ...baseArtist, listen_count: 42 }], enrichment);
		expect(result[0].listen_count).toBe(42);
	});
});

describe('applyAlbumEnrichment', () => {
	const baseAlbum: Album = {
		musicbrainz_id: 'alb-1',
		title: 'Absolution',
		artist: null,
		year: null,
		in_library: false,
		track_count: null,
		listen_count: null
	};

	it('applies enrichment data to matching albums', () => {
		expect.assertions(1);
		const enrichment: EnrichmentResponseV3 = {
			artists: [],
			degradations: [],
			albums: [{ musicbrainz_id: 'alb-1', track_count: 12, listen_count: 80000 }],
			source: 'lastfm'
		};
		const result = applyAlbumEnrichment([baseAlbum], enrichment);
		expect(result[0].listen_count).toBe(80000);
	});

	it('preserves zero listen_count from enrichment', () => {
		expect.assertions(1);
		const enrichment: EnrichmentResponseV3 = {
			artists: [],
			degradations: [],
			albums: [{ musicbrainz_id: 'alb-1', track_count: null, listen_count: 0 }],
			source: 'lastfm'
		};
		const result = applyAlbumEnrichment([{ ...baseAlbum, listen_count: 500 }], enrichment);
		expect(result[0].listen_count).toBe(0);
	});

	it('merges arrived counts while a degradation note rides along', () => {
		expect.assertions(2);
		const enrichment: EnrichmentResponseV3 = {
			artists: [],
			degradations: [{ source: 'lastfm', code: 'ENRICHMENT_UNAVAILABLE', message: 'down' }],
			albums: [{ musicbrainz_id: 'alb-1', track_count: 12, listen_count: 80000 }],
			source: 'listenbrainz'
		};
		const result = applyAlbumEnrichment([baseAlbum], enrichment);
		expect(result[0].listen_count).toBe(80000);
		expect(result[0].track_count).toBe(12);
	});
});
