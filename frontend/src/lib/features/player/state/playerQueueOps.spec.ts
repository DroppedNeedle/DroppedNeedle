import { describe, it, expect } from 'vitest';
import type { QueueItem } from '$lib/features/player/playback/types';
import {
	queueHasAlbums,
	computeAllowedIndex,
	computeNextAlbumIndex,
	computeNextIndex,
	computePreviousAlbumIndex,
	computePreviousIndex
} from './playerQueueOps';

// Minimal queue items - the album helpers only read `albumId`.
const q = (albumId: string): QueueItem => ({ albumId }) as QueueItem;
const build = (...albumIds: string[]): QueueItem[] => albumIds.map(q);

describe('queueHasAlbums', () => {
	it('is true only when two consecutive tracks share an albumId', () => {
		const cases: [string, QueueItem[], boolean][] = [
			['an empty queue', [], false],
			['a single track', build('a'), false],
			['unrelated singles', build('a', 'b', 'c'), false],
			['tracks with no albumId', build('', '', ''), false],
			['two of one album in a row', build('a', 'a', 'b'), true]
		];
		for (const [what, queue, expected] of cases) {
			expect(queueHasAlbums(queue), what).toBe(expected);
		}
	});
});

describe('computeNextAlbumIndex', () => {
	it('jumps to the next album block, with no-albumId tracks as solo blocks', () => {
		const albums = build('a', 'a', 'b', 'b');
		const cases: [QueueItem[], number, number | null][] = [
			// Out of range.
			[build('a', 'a'), 5, null],
			[albums, 0, 2],
			[albums, 1, 2],
			// On the last album.
			[albums, 2, null],
			[albums, 3, null],
			[build('', ''), 0, 1]
		];
		for (const [queue, index, expected] of cases) {
			expect(computeNextAlbumIndex(queue, index), `${index}`).toBe(expected);
		}
	});
});

describe('computePreviousAlbumIndex', () => {
	it('restarts the album mid-way, else goes to the one before, and stops at the start', () => {
		const albums = build('a', 'a', 'b', 'b');
		const cases: [QueueItem[], number, number | null][] = [
			[build('a', 'a', 'a', 'b'), 2, 0],
			[albums, 2, 0],
			[albums, 3, 2],
			[albums, 0, null]
		];
		for (const [queue, index, expected] of cases) {
			expect(computePreviousAlbumIndex(queue, index), `${index}`).toBe(expected);
		}
	});
});

describe('computeAllowedIndex', () => {
	const allowed = new Set([0, 3, 4]);
	const allows = (index: number) => allowed.has(index);

	it('steps forward over the indexes it may not land on', () => {
		expect(computeAllowedIndex(computeNextIndex, 0, 5, false, [], allows)).toBe(3);
		expect(computeAllowedIndex(computeNextIndex, 3, 5, false, [], allows)).toBe(4);
	});

	it('steps back the same way', () => {
		expect(computeAllowedIndex(computePreviousIndex, 4, 5, false, [], allows)).toBe(3);
		expect(computeAllowedIndex(computePreviousIndex, 3, 5, false, [], allows)).toBe(0);
	});

	it('follows the shuffle order', () => {
		expect(computeAllowedIndex(computeNextIndex, 0, 5, true, [0, 1, 4, 2, 3], allows)).toBe(4);
	});

	it('ends when nothing further is allowed', () => {
		expect(computeAllowedIndex(computeNextIndex, 4, 5, false, [], allows)).toBeNull();
		expect(computeAllowedIndex(computeNextIndex, 0, 3, false, [], allows)).toBeNull();
	});
});
