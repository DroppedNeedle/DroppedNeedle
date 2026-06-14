import { describe, it, expect } from 'vitest';
import { dedupeById } from './dedupe';

describe('dedupeById', () => {
	it('removes entries with duplicate ids, keeping the first occurrence', () => {
		const result = dedupeById([
			{ id: 'a', title: 'first' },
			{ id: 'b', title: 'second' },
			{ id: 'a', title: 'duplicate of first' }
		]);
		expect(result).toEqual([
			{ id: 'a', title: 'first' },
			{ id: 'b', title: 'second' }
		]);
	});

	it('drops entries with missing ids that would collide as keys', () => {
		const result = dedupeById([
			{ id: 'a' },
			{ id: '' },
			{ id: null },
			{ id: undefined },
			{ id: 'b' }
		]);
		expect(result).toEqual([{ id: 'a' }, { id: 'b' }]);
	});

	it('returns an empty array unchanged', () => {
		expect(dedupeById([])).toEqual([]);
	});

	it('preserves order for already-unique input', () => {
		const input = [{ id: 'x' }, { id: 'y' }, { id: 'z' }];
		expect(dedupeById(input)).toEqual(input);
	});
});
