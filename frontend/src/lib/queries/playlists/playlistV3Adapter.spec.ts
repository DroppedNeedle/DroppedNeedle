import { describe, expect, it } from 'vitest';

import {
	epochSecToIso,
	toPageDetail,
	toPageList,
	toPageListItem,
	toPageSummary,
	toPageTrack,
	trackDataToV3Input,
	transposeMembership,
	type PlaylistDetailV3,
	type PlaylistSummaryV3,
	type PlaylistTrackV3
} from './playlistV3Adapter';

function makeTrackV3(overrides: Partial<PlaylistTrackV3> = {}): PlaylistTrackV3 {
	return {
		id: 'trk-1',
		position: 0,
		track_name: 'Song',
		artist_name: 'Singer',
		album_name: 'Album',
		source_type: 'local',
		created_at: 1767225600,
		...overrides
	};
}

function makeSummaryV3(overrides: Partial<PlaylistSummaryV3> = {}): PlaylistSummaryV3 {
	return {
		id: 'pl-1',
		name: 'Mix',
		track_count: 2,
		cover_urls: [],
		created_at: 1767225600,
		updated_at: 1767312000,
		is_public: false,
		is_owner: true,
		is_redacted: false,
		...overrides
	};
}

function makeDetailV3(overrides: Partial<PlaylistDetailV3> = {}): PlaylistDetailV3 {
	return {
		...makeSummaryV3(),
		tracks: [makeTrackV3()],
		...overrides
	};
}

describe('playlistV3Adapter', () => {
	it('converts epoch seconds to ISO dates', () => {
		expect(epochSecToIso(1767225600)).toBe('2026-01-01T00:00:00.000Z');
	});

	it('adapts a summary with dates and provenance', () => {
		const page = toPageSummary(
			makeSummaryV3({ source_ref: 'spotify:abc', total_duration: 900, owner_name: 'Ada' })
		);

		expect(page.source_ref).toBe('spotify:abc');
		expect(page.created_at).toBe('2026-01-01T00:00:00.000Z');
		expect(page.updated_at).toBe('2026-01-02T00:00:00.000Z');
		expect(page.total_duration).toBe(900);
		expect(page.owner_name).toBe('Ada');
		expect(page.is_redacted).toBe(false);
	});

	it('nulls missing optional summary fields', () => {
		const page = toPageSummary(makeSummaryV3());

		expect(page.source_ref).toBeNull();
		expect(page.total_duration).toBeNull();
		expect(page.custom_cover_url).toBeNull();
		expect(page.owner_name).toBeNull();
	});

	it('adapts a detail with its tracks', () => {
		const page = toPageDetail(makeDetailV3({ source_ref: 'plex:xyz' }));

		expect(page.source_ref).toBe('plex:xyz');
		expect(page.tracks).toHaveLength(1);
		expect(page.tracks[0].track_name).toBe('Song');
		expect(page.tracks[0].created_at).toBe('2026-01-01T00:00:00.000Z');
	});

	it('adapts a track row fully', () => {
		const page = toPageTrack(
			makeTrackV3({
				album_id: 'alb-1',
				duration: 240,
				available_sources: ['local'],
				format: 'flac'
			})
		);

		expect(page.album_id).toBe('alb-1');
		expect(page.duration).toBe(240);
		expect(page.available_sources).toEqual(['local']);
		expect(page.format).toBe('flac');
		expect(page.artist_id).toBeNull();
		expect(page.library_file_id).toBeNull();
	});

	it('passes redacted rows through without provenance', () => {
		const page = toPageListItem({
			id: 'pl-x',
			track_count: 7,
			owner_name: 'Cara',
			is_redacted: true
		});

		expect(page).toEqual({
			id: 'pl-x',
			track_count: 7,
			owner_name: 'Cara',
			is_redacted: true
		});
	});

	it('adapts a mixed list', () => {
		const pages = toPageList([
			makeSummaryV3({ id: 'pl-1' }),
			{ id: 'pl-x', track_count: 4, owner_name: null, is_redacted: true }
		]);

		expect(pages).toHaveLength(2);
		expect(pages[0].is_redacted).toBe(false);
		expect(pages[1].is_redacted).toBe(true);
	});

	it('converts page track payloads to V3 inputs', () => {
		const input = trackDataToV3Input({
			track_name: 'Song',
			artist_name: 'Singer',
			album_name: 'Album',
			source_type: 'local',
			duration: 240
		});

		expect(input).toMatchObject({
			track_name: 'Song',
			artist_name: 'Singer',
			album_name: 'Album',
			source_type: 'local',
			duration: 240
		});
		expect(input.album_id).toBeNull();
	});

	it('transposes V3 membership to playlist-first', () => {
		expect(transposeMembership({ '0': ['pl-1', 'pl-2'], '1': ['pl-1'], '2': [] })).toEqual({
			'pl-1': [0, 1],
			'pl-2': [0]
		});
	});

	it('drops non-numeric membership keys', () => {
		expect(transposeMembership({ nope: ['pl-1'], '0': ['pl-1'] })).toEqual({ 'pl-1': [0] });
	});
});
