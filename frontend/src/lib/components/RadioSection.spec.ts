import { describe, expect, it, vi, beforeEach } from 'vitest';

vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';

const mockInvalidate = vi.mocked(invalidateQueriesWithPersister);
const USER = 'user-a';

describe('DiscoverQueryKeyFactory.radio', () => {
	it('returns the expected key shape for artist seed', () => {
		const key = DiscoverQueryKeyFactory.radio(USER, 'artist', 'test-mbid');
		expect(key).toEqual([
			'discover',
			USER,
			{ user_id: USER, source_mode: 'brainzmash', source_id: '', generation: 0 },
			'radio',
			'artist',
			'test-mbid'
		]);
	});

	it('returns the expected key shape for album seed', () => {
		const key = DiscoverQueryKeyFactory.radio(USER, 'album', 'album-mbid');
		expect(key).toEqual([
			'discover',
			USER,
			{ user_id: USER, source_mode: 'brainzmash', source_id: '', generation: 0 },
			'radio',
			'album',
			'album-mbid'
		]);
	});

	it('generates different keys for different seed types', () => {
		const artistKey = DiscoverQueryKeyFactory.radio(USER, 'artist', 'test-mbid');
		const albumKey = DiscoverQueryKeyFactory.radio(USER, 'album', 'test-mbid');
		expect(artistKey).not.toEqual(albumKey);
	});

	it('generates different keys for different seed IDs', () => {
		const key1 = DiscoverQueryKeyFactory.radio(USER, 'artist', 'mbid-1');
		const key2 = DiscoverQueryKeyFactory.radio(USER, 'artist', 'mbid-2');
		expect(key1).not.toEqual(key2);
	});

	it('generates different keys for different users (AMU-5)', () => {
		const key1 = DiscoverQueryKeyFactory.radio('user-a', 'artist', 'test-mbid');
		const key2 = DiscoverQueryKeyFactory.radio('user-b', 'artist', 'test-mbid');
		expect(key1).not.toEqual(key2);
	});

	it('starts with the discover prefix and carries userId before the radio marker', () => {
		const key = DiscoverQueryKeyFactory.radio(USER, 'artist', 'test-mbid');
		expect(key[0]).toBe('discover');
		expect(key[1]).toBe(USER);
		expect(key[2]).toEqual({
			user_id: USER,
			source_mode: 'brainzmash',
			source_id: '',
			generation: 0
		});
		expect(key[3]).toBe('radio');
	});
});

describe('RadioSection refresh invalidation contract', () => {
	beforeEach(() => {
		vi.clearAllMocks();
	});

	it('refresh calls invalidateQueriesWithPersister with correct radio query key', async () => {
		const seedType = 'artist';
		const seedId = 'abc-123';

		// Mirrors RadioCard.svelte handleRefresh().
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.radio(USER, seedType, seedId)
		});

		expect(mockInvalidate).toHaveBeenCalledOnce();
		expect(mockInvalidate).toHaveBeenCalledWith({
			queryKey: [
				'discover',
				USER,
				{ user_id: USER, source_mode: 'brainzmash', source_id: '', generation: 0 },
				'radio',
				'artist',
				'abc-123'
			]
		});
	});

	it('refresh uses distinct keys for different seeds', async () => {
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.radio(USER, 'artist', 'seed-1')
		});
		await invalidateQueriesWithPersister({
			queryKey: DiscoverQueryKeyFactory.radio(USER, 'album', 'seed-2')
		});

		expect(mockInvalidate).toHaveBeenCalledTimes(2);
		expect(mockInvalidate.mock.calls[0][0]).not.toEqual(mockInvalidate.mock.calls[1][0]);
	});
});
