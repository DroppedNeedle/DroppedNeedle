import { describe, expect, it } from 'vitest';
import { heldTracksByGroup, trackCountLabel } from './trackCount';

describe('trackCountLabel', () => {
	it('names the edition count when known', () => {
		expect(trackCountLabel(4, 12)).toBe('4/12 tracks');
		expect(trackCountLabel(1, 1)).toBe('1/1 track');
	});

	it('names the held count alone when the edition count is unknown', () => {
		expect(trackCountLabel(4, null)).toBe('4 tracks');
		expect(trackCountLabel(1)).toBe('1 track');
	});
});

describe('heldTracksByGroup', () => {
	it('adds copies of one group up and leaves out albums without a group or files', () => {
		const held = heldTracksByGroup([
			{ release_group_mbid: 'RG-1', track_count: 5, expected_track_count: 11 },
			{ release_group_mbid: 'rg-1', track_count: 6, expected_track_count: null },
			{ release_group_mbid: 'rg-2', track_count: 0, expected_track_count: 4 },
			{ release_group_mbid: null, track_count: 3 }
		]);
		expect([...held]).toEqual([['rg-1', { held: 11, expected: 11 }]]);
	});

	it('counts a second full copy no further than the edition', () => {
		const held = heldTracksByGroup([
			{ release_group_mbid: 'rg-1', track_count: 11, expected_track_count: 11 },
			{ release_group_mbid: 'rg-1', track_count: 5, expected_track_count: 11 }
		]);
		expect(held.get('rg-1')).toEqual({ held: 11, expected: 11 });
	});
});
