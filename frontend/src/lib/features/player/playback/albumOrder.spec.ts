import { describe, expect, it } from 'vitest';
import { followsOnAlbum } from './albumOrder';
import type { QueueItem } from './types';

const at = (albumId: string, trackNumber: number, discNumber?: number) =>
	({ albumId, trackNumber, discNumber }) as QueueItem;

describe('followsOnAlbum', () => {
	it.each([
		['the next track', at('a', 3), at('a', 4), true],
		['a track further on', at('a', 3), at('a', 5), false],
		['the first track of the next disc', at('a', 12, 1), at('a', 1, 2), true],
		['a disc missing as disc 1', at('a', 3), at('a', 4, 1), true],
		['unknown track numbers', at('a', 0), at('a', 0), true],
		['another album', at('a', 3), at('b', 4), false],
		['no album', at('', 3), at('', 4), false]
	])('%s', (_, previous, next, expected) => {
		expect(followsOnAlbum(previous, next)).toBe(expected);
	});
});
