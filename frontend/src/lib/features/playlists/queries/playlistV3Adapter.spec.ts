import { describe, expect, it } from 'vitest';

import {
	epochSecToIso,
	privatePlaylistLine,
	toPageListItem,
	transposeMembership
} from './playlistV3Adapter';

describe('playlistV3Adapter', () => {
	it('converts dates and membership, passing redacted rows through', () => {
		{
			// Converts epoch seconds to ISO dates.
			expect(epochSecToIso(1767225600)).toBe('2026-01-01T00:00:00.000Z');
		}

		{
			// Passes redacted rows through without provenance.
			const page = toPageListItem({
				id: 'pl-x',
				track_count: 7,
				owner_name: 'Cara',
				can_delete: false,
				is_redacted: true
			});

			expect(page).toEqual({
				id: 'pl-x',
				track_count: 7,
				owner_name: 'Cara',
				can_delete: false,
				is_redacted: true
			});
		}

		{
			// Names a locked playlist's owner only when the server sends a name.
			expect(privatePlaylistLine({ track_count: 4, owner_name: 'Cara', can_delete: false })).toBe(
				'4 songs · By Cara'
			);
			expect(privatePlaylistLine({ track_count: 1, owner_name: null, can_delete: false })).toBe(
				'1 song'
			);
			expect(privatePlaylistLine({ track_count: 2, owner_name: null, can_delete: true })).toBe(
				'2 songs · No owner'
			);
		}

		{
			// Transposes V3 membership to playlist-first.
			expect(transposeMembership({ '0': ['pl-1', 'pl-2'], '1': ['pl-1'], '2': [] })).toEqual({
				'pl-1': [0, 1],
				'pl-2': [0]
			});
		}

		{
			// Drops non-numeric membership keys.
			expect(transposeMembership({ nope: ['pl-1'], '0': ['pl-1'] })).toEqual({ 'pl-1': [0] });
		}
	});
});
