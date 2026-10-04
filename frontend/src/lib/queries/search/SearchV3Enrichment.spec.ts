import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Album, Artist } from '$lib/types';
import {
	ENRICH_COLLECTOR_DELAY_MS,
	MAX_ENRICH_IDS_PER_BUCKET,
	SearchEnrichCollector
} from './SearchV3Enrichment.svelte';

const artist = (id: string, overrides: Partial<Artist> = {}): Artist => ({
	title: `Artist ${id}`,
	musicbrainz_id: id,
	in_library: false,
	...overrides
});

const album = (id: string, overrides: Partial<Album> = {}): Album => ({
	title: `Album ${id}`,
	artist: 'Artist',
	year: null,
	musicbrainz_id: id,
	in_library: false,
	...overrides
});

describe('SearchEnrichCollector', () => {
	beforeEach(() => vi.useFakeTimers());
	afterEach(() => vi.useRealTimers());

	it('starts with an empty body that keeps the batch query disabled', () => {
		expect.assertions(1);
		const collector = new SearchEnrichCollector();
		expect(collector.body).toEqual({ artists: [], albums: [] });
		collector.dispose();
	});

	it('debounces a burst of hovers into one cumulative body', () => {
		expect.assertions(3);
		const collector = new SearchEnrichCollector();
		collector.requestArtist(artist('a-1'));
		collector.requestArtist(artist('a-2'));
		collector.requestAlbum(album('b-1'));
		expect(collector.body).toEqual({ artists: [], albums: [] });

		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		expect(collector.body.artists?.map((item) => item.musicbrainz_id)).toEqual(['a-1', 'a-2']);
		expect(collector.body.albums?.map((item) => item.musicbrainz_id)).toEqual(['b-1']);
		collector.dispose();
	});

	it('accumulates later hovers into the committed body', () => {
		expect.assertions(1);
		const collector = new SearchEnrichCollector();
		collector.requestArtist(artist('a-1'));
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		collector.requestArtist(artist('a-2'));
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		expect(collector.body.artists?.map((item) => item.musicbrainz_id)).toEqual(['a-1', 'a-2']);
		collector.dispose();
	});

	it('skips rows that are already enriched or carry no provider id', () => {
		expect.assertions(1);
		const collector = new SearchEnrichCollector();
		collector.requestArtist(artist('a-1', { listen_count: 42 }));
		collector.requestArtist(artist('local-1', { local_id: 'local-1' }));
		collector.requestAlbum(album('local-2', { local_id: 'local-2' }));
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		expect(collector.body).toEqual({ artists: [], albums: [] });
		collector.dispose();
	});

	it('caps each bucket at the server limit', () => {
		expect.assertions(2);
		const collector = new SearchEnrichCollector();
		for (let index = 0; index < MAX_ENRICH_IDS_PER_BUCKET + 4; index += 1) {
			collector.requestArtist(artist(`a-${index}`));
		}
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		expect(collector.body.artists).toHaveLength(MAX_ENRICH_IDS_PER_BUCKET);
		collector.requestArtist(artist('a-late'));
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		expect(collector.body.artists).toHaveLength(MAX_ENRICH_IDS_PER_BUCKET);
		collector.dispose();
	});

	it('resets pending and committed ids for a new query', () => {
		expect.assertions(1);
		const collector = new SearchEnrichCollector();
		collector.requestArtist(artist('a-1'));
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		collector.requestArtist(artist('a-2'));
		collector.reset();
		vi.advanceTimersByTime(ENRICH_COLLECTOR_DELAY_MS);
		expect(collector.body).toEqual({ artists: [], albums: [] });
		collector.dispose();
	});
});
