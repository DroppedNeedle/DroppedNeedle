import { describe, expect, it } from 'vitest';

import { albumViewToSummary } from './LibraryV3Adapters';
import type { AlbumViewV3 } from './LibraryV3Queries.svelte';

const view: AlbumViewV3 = {
	id: 'al1',
	title: 'First Light',
	artist_name: 'Aurora',
	artist_id: 'a1',
	release_group_mbid: 'rg1',
	release_mbid: 'r1',
	artist_mbid: 'm1',
	identity_state: 'linked',
	track_count: 2,
	total_duration_seconds: 400,
	total_size_bytes: 8000000,
	format: 'flac',
	year: 1994,
	is_compilation: false,
	cover_available: true,
	date_added: 1000,
	favorite: false
};

describe('LibraryV3Adapters', () => {
	it('maps catalog views onto the shared card shape', () => {
		const summary = albumViewToSummary(view);
		expect(summary.id).toBe('al1');
		expect(summary.title).toBe('First Light');
		expect(summary.musicbrainz_release_group_id).toBe('rg1');
		expect(summary.musicbrainz_release_id).toBe('r1');
		expect(summary.musicbrainz_artist_id).toBe('m1');
		expect(summary.track_count).toBe(2);
		expect(summary.format).toBe('flac');
		expect(summary.cover_available).toBe(true);
	});

	it('grades identity from linked state and release precision', () => {
		expect(albumViewToSummary(view).album_identity_state).toBe('release_linked');
		expect(albumViewToSummary({ ...view, release_mbid: null }).album_identity_state).toBe(
			'release_group_linked'
		);
		expect(albumViewToSummary({ ...view, identity_state: 'local_only' }).album_identity_state).toBe(
			'local_only'
		);
	});
});
