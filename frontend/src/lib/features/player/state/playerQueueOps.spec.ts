import { describe, it, expect } from 'vitest';
import type { QueueItem } from '$lib/features/player/playback/types';
import { queueHasAlbums, computeNextAlbumIndex, computePreviousAlbumIndex } from './playerQueueOps';

// Minimal queue items - the album helpers only read `albumId`.
const q = (albumId: string): QueueItem => ({ albumId }) as QueueItem;
const build = (...albumIds: string[]): QueueItem[] => albumIds.map(q);

describe('queueHasAlbums', () => {
	it('is false for an empty queue', () => {
		expect(queueHasAlbums([])).toBe(false);
	});

	it('is false for a single track', () => {
		expect(queueHasAlbums(build('a'))).toBe(false);
	});

	it('is false for unrelated singles (each a different album)', () => {
		expect(queueHasAlbums(build('a', 'b', 'c'))).toBe(false);
	});

	it('is false for tracks with no albumId', () => {
		expect(queueHasAlbums(build('', '', ''))).toBe(false);
	});

	it('is true when two consecutive tracks share an albumId', () => {
		expect(queueHasAlbums(build('a', 'a', 'b'))).toBe(true);
	});
});

describe('computeNextAlbumIndex', () => {
	it('returns null for an out-of-range index', () => {
		expect(computeNextAlbumIndex(build('a', 'a'), 5)).toBeNull();
	});

	it('jumps to the start of the next album block', () => {
		const queue = build('a', 'a', 'b', 'b');
		expect(computeNextAlbumIndex(queue, 0)).toBe(2);
		expect(computeNextAlbumIndex(queue, 1)).toBe(2);
	});

	it('returns null when on the last album', () => {
		const queue = build('a', 'a', 'b', 'b');
		expect(computeNextAlbumIndex(queue, 2)).toBeNull();
		expect(computeNextAlbumIndex(queue, 3)).toBeNull();
	});

	it('treats no-albumId tracks as solo blocks', () => {
		const queue = build('', '');
		expect(computeNextAlbumIndex(queue, 0)).toBe(1);
	});
});

describe('computePreviousAlbumIndex', () => {
	it('restarts the current album when playing mid-album', () => {
		const queue = build('a', 'a', 'a', 'b');
		expect(computePreviousAlbumIndex(queue, 2)).toBe(0);
	});

	it('jumps to the previous album when at the start of the current one', () => {
		const queue = build('a', 'a', 'b', 'b');
		expect(computePreviousAlbumIndex(queue, 2)).toBe(0);
		expect(computePreviousAlbumIndex(queue, 3)).toBe(2);
	});

	it('returns null at the very start of the queue', () => {
		const queue = build('a', 'a', 'b', 'b');
		expect(computePreviousAlbumIndex(queue, 0)).toBeNull();
	});
});
