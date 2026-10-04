import { describe, expect, it } from 'vitest';

import {
	albumCardToSummary,
	albumCoverUrl,
	crateCoverUrl,
	localCoverUrl,
	matchTrackToQueueItem,
	suggestionToCrateTrack
} from './LocalV3Adapters';
import type { AlbumCardV3, LocalTrackV3, SuggestionTrackV3 } from './LocalV3Queries.svelte';

const card: AlbumCardV3 = {
	id: 'al1',
	title: 'First Light',
	artist_name: 'Aurora',
	artist_mbid: 'm1',
	release_group_mbid: 'rg1',
	year: 1994,
	track_count: 2,
	total_size_bytes: 8000000,
	primary_format: 'flac',
	cover_available: true,
	date_added: 1000
};

const suggestion: SuggestionTrackV3 = {
	track_id: 't1',
	title: 'Opener',
	album_title: 'First Light',
	artist_name: 'Aurora',
	album_id: 'al1',
	cover_available: true,
	format: 'flac',
	year: 1994,
	duration_seconds: 200,
	reason: 'recent'
};

const track: LocalTrackV3 = {
	id: 't1',
	title: 'Opener',
	album_id: 'al1',
	album_title: 'First Light',
	artist_name: 'Aurora',
	artist_id: 'a1',
	album_artist_name: 'Aurora',
	disc_number: 1,
	track_number: 1,
	year: 1994,
	genre: null,
	duration_seconds: 200,
	format: 'FLAC',
	bit_rate: null,
	sample_rate: null,
	file_size_bytes: 4000000,
	date_added: 1000,
	cover_available: true,
	favorite: false
};

describe('LocalV3Adapters', () => {
	it('builds v3 cover urls from the release-group mbid only', () => {
		expect(localCoverUrl('rg1')).toBe('/api/v3/covers/release-group/rg1?size=250');
		expect(localCoverUrl('rg1', 500)).toBe('/api/v3/covers/release-group/rg1?size=500');
		expect(localCoverUrl(null)).toBeNull();
		expect(localCoverUrl(undefined)).toBeNull();
	});

	it('keys album summaries by mbid with the local id as fallback', () => {
		const linked = albumCardToSummary(card);
		expect(linked.musicbrainz_id).toBe('rg1');
		expect(linked.name).toBe('First Light');
		expect(linked.cover_url).toBe('/api/v3/covers/release-group/rg1?size=250');
		expect(linked.primary_format).toBe('flac');
		expect(linked.date_added).toBe(new Date(1000 * 1000).toISOString());

		const localOnly = albumCardToSummary({ ...card, release_group_mbid: null });
		expect(localOnly.musicbrainz_id).toBe('al1');
		expect(localOnly.cover_url).toBeNull();
	});

	it('adapts suggestions with caller-supplied album mbids and safe reasons', () => {
		const adapted = suggestionToCrateTrack(suggestion, 'rg1');
		expect(adapted.track_file_id).toBe('t1');
		expect(adapted.album_mbid).toBe('rg1');
		expect(adapted.cover_url).toBe('/api/v3/covers/release-group/rg1?size=250');
		expect(adapted.reason).toBe('recent');

		const unknown = suggestionToCrateTrack({ ...suggestion, reason: 'match' }, null);
		expect(unknown.album_mbid).toBeNull();
		expect(unknown.cover_url).toBeNull();
		expect(unknown.reason).toBe('surprise');
	});

	it('resolves crate and album covers with an mbid fallback', () => {
		expect(crateCoverUrl({ cover_url: null, album_mbid: 'rg1' })).toContain(
			'/api/v3/covers/release-group/rg1'
		);
		expect(crateCoverUrl({ cover_url: null, album_mbid: null })).toBeNull();
		expect(albumCoverUrl({ cover_url: null, musicbrainz_id: 'rg1' })).toContain(
			'/api/v3/covers/release-group/rg1'
		);
		expect(albumCoverUrl({ cover_url: null, musicbrainz_id: 'al1' })).toContain(
			'/api/v3/covers/release-group/al1'
		);
	});

	it('builds playable queue items from matched tracks keyed by track id', () => {
		const item = matchTrackToQueueItem(track, 'https://covers/rg1', 'rg1');
		expect(item.trackSourceId).toBe('t1');
		expect(item.sourceType).toBe('local');
		expect(item.streamUrl).toBe('/api/v3/stream/local/t1');
		expect(item.coverUrl).toBe('https://covers/rg1');
		expect(item.format).toBe('flac');
		expect(item.artistId).toBe('a1');
		expect(item.duration).toBe(200);
	});
});
