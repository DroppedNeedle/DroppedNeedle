import { describe, expect, it } from 'vitest';

import { epochSecToIso, toPageListItem, transposeMembership } from './playlistV3Adapter';

describe('playlistV3Adapter', () => {
	it('converts epoch seconds to ISO dates', () => {
		expect(epochSecToIso(1767225600)).toBe('2026-01-01T00:00:00.000Z');
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
