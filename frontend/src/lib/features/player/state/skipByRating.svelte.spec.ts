import { beforeEach, describe, expect, it } from 'vitest';
import type { QueueItem } from '$lib/features/player/playback/types';
import { libraryTrackId, skipByRating } from './skipByRating.svelte';

const item = (overrides: Partial<QueueItem>): QueueItem => ({
	trackSourceId: 'local-1',
	trackName: 'Song',
	artistName: 'Artist',
	trackNumber: 1,
	albumId: 'album-1',
	albumName: 'Album',
	coverUrl: null,
	sourceType: 'local',
	...overrides
});

beforeEach(() => {
	skipByRating.set(0);
	skipByRating.ratings = null;
});

describe('skipByRating', () => {
	it('lets every track through while it is off or the ratings are not read', () => {
		expect(skipByRating.allows(item({}))).toBe(true);
		skipByRating.set(4);
		expect(skipByRating.allows(item({}))).toBe(true);
	});

	it('skips library tracks rated under the floor, unrated ones too', () => {
		skipByRating.set(4);
		skipByRating.ratings = { 'local-1': 5, 'local-2': 3 };
		expect(skipByRating.allows(item({ trackSourceId: 'local-1' }))).toBe(true);
		expect(skipByRating.allows(item({ trackSourceId: 'local-2' }))).toBe(false);
		expect(skipByRating.allows(item({ trackSourceId: 'local-3' }))).toBe(false);
	});

	it('plays tracks that cannot be rated', () => {
		skipByRating.set(4);
		skipByRating.ratings = {};
		expect(skipByRating.allows(item({ sourceType: 'youtube', trackSourceId: 'yt-1' }))).toBe(true);
	});

	it('judges a track by its library copy when it plays from elsewhere', () => {
		const remote = item({
			sourceType: 'jellyfin',
			trackSourceId: 'jf-1',
			sourceIds: { local: 'local-2' }
		});
		expect(libraryTrackId(remote)).toBe('local-2');
		skipByRating.set(3);
		skipByRating.ratings = { 'local-2': 2 };
		expect(skipByRating.allows(remote)).toBe(false);
	});

	it('keeps the floor in the browser and forgets it when switched off', () => {
		skipByRating.set(3);
		expect(localStorage.getItem('player.skipBelowStars')).toBe('3');
		skipByRating.set(0);
		expect(localStorage.getItem('player.skipBelowStars')).toBeNull();
	});
});
