import { describe, expect, it } from 'vitest';
import { trackCountLabel } from './trackCount';

describe('trackCountLabel', () => {
	it('names the edition count while tracks are missing', () => {
		expect(trackCountLabel(4, 12)).toBe('4/12 tracks');
		expect(trackCountLabel(0, 1)).toBe('0/1 track');
	});

	it('names the held count alone once the album is complete', () => {
		expect(trackCountLabel(14, 14)).toBe('14 tracks');
		expect(trackCountLabel(1, 1)).toBe('1 track');
	});

	it('names the held count alone when the edition count is unknown', () => {
		expect(trackCountLabel(4, null)).toBe('4 tracks');
		expect(trackCountLabel(1)).toBe('1 track');
	});
});
