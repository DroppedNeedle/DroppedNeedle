import { describe, expect, it } from 'vitest';
import { trackCountLabel } from './trackCount';

describe('trackCountLabel', () => {
	it('names the edition count while tracks are missing, else the held count alone', () => {
		const cases: [number, number | null | undefined, string][] = [
			[4, 12, '4/12 tracks'],
			[0, 1, '0/1 track'],
			// Complete.
			[14, 14, '14 tracks'],
			[1, 1, '1 track'],
			// Edition count unknown.
			[4, null, '4 tracks'],
			[1, undefined, '1 track']
		];
		for (const [held, total, expected] of cases) {
			expect(trackCountLabel(held, total)).toBe(expected);
		}
	});
});
