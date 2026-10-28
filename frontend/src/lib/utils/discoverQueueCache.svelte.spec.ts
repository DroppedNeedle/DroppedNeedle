import { beforeEach, describe, expect, it, vi } from 'vitest';
import { CACHE_KEYS, CACHE_TTL } from '$lib/constants';
import {
	getQueueCachedData,
	removeQueueCachedData,
	setQueueCachedData,
	subscribeQueueCacheChanges,
	updateDiscoverQueueCacheTTL
} from './discoverQueueCache';

const { source } = vi.hoisted(() => ({
	source: { source_mode: 'brainzmash', source_id: 'opaque-a', generation: 1 }
}));
vi.mock('$lib/queries/musicbrainz/sourceScope.svelte', () => ({
	musicBrainzSourceKey: (userId: string) => ({ user_id: userId, ...source })
}));
const USER_A = 'user-a';
const USER_B = 'user-b';

describe('discoverQueueCache', () => {
	beforeEach(() => {
		localStorage.clear();
		updateDiscoverQueueCacheTTL(CACHE_TTL.DISCOVER_QUEUE);
		vi.restoreAllMocks();
		source.source_id = 'opaque-a';
		source.generation = 1;
	});

	it('stores and retrieves queue items with enrichment payload', () => {
		expect.assertions(3);
		setQueueCachedData(
			{
				items: [
					{
						musicbrainz_id: 'rg-1',
						title: 'Album 1',
						artist_name: 'Artist 1',
						artist_id: 'artist-1',
						recommendation_reason: 'reason',
						is_wildcard: false,
						in_library: false,
						enrichment: {
							artist_mbid: 'artist-1',
							release_date: '1970-01-01',
							country: 'GB',
							tags: ['prog rock'],
							youtube_url: null,
							youtube_search_url: 'https://youtube.example',
							youtube_search_available: true,
							artist_description: 'desc',
							listen_count: 10
						}
					}
				],
				currentIndex: 0,
				queueId: 'queue-1'
			},
			USER_A
		);

		const cached = getQueueCachedData(USER_A);
		expect(cached).not.toBeNull();
		expect(cached?.data.items[0].enrichment?.release_date).toBe('1970-01-01');
		expect(cached?.data.queueId).toBe('queue-1');
	});

	it('invalidates stale queue cache entries on read', () => {
		expect.assertions(2);
		updateDiscoverQueueCacheTTL(10);
		vi.spyOn(Date, 'now').mockReturnValue(1000);
		setQueueCachedData(
			{
				items: [],
				currentIndex: 0,
				queueId: 'queue-2'
			},
			USER_A
		);

		vi.spyOn(Date, 'now').mockReturnValue(1200);
		const cached = getQueueCachedData(USER_A);
		expect(cached).toBeNull();
		expect(localStorage.getItem(`${CACHE_KEYS.DISCOVER_QUEUE}_${USER_A}`)).toBeNull();
	});

	it('emits cache change events for same-tab updates', () => {
		expect.assertions(1);
		let events = 0;
		const unsubscribe = subscribeQueueCacheChanges(() => {
			events++;
		});

		setQueueCachedData(
			{
				items: [],
				currentIndex: 0,
				queueId: 'queue-3'
			},
			USER_A
		);
		removeQueueCachedData(USER_A);
		unsubscribe();

		expect(events).toBe(2);
	});

	it('isolates cache entries per user (no cross-user leak)', () => {
		expect.assertions(3);
		setQueueCachedData({ items: [], currentIndex: 0, queueId: 'queue-a' }, USER_A);
		setQueueCachedData({ items: [], currentIndex: 0, queueId: 'queue-b' }, USER_B);

		expect(getQueueCachedData(USER_A)?.data.queueId).toBe('queue-a');
		expect(getQueueCachedData(USER_B)?.data.queueId).toBe('queue-b');
		expect(getQueueCachedData('user-c')).toBeNull();
	});
	it('rejects legacy envelopes without provenance', () => {
		localStorage.setItem(
			`${CACHE_KEYS.DISCOVER_QUEUE}_${USER_A}`,
			JSON.stringify({
				data: { items: [], currentIndex: 0, queueId: 'legacy' },
				timestamp: Date.now()
			})
		);
		expect(getQueueCachedData(USER_A)).toBeNull();
	});

	it('does not revive an old A queue after A to B to A, even without reading in B', () => {
		setQueueCachedData({ items: [], currentIndex: 0, queueId: 'old-a' }, USER_A);
		source.source_id = 'opaque-b';
		source.generation = 2;
		source.source_id = 'opaque-a-new';
		source.generation = 3;
		expect(getQueueCachedData(USER_A)).toBeNull();
	});

	it('rejects writes before source identity is known', () => {
		source.source_id = '';
		setQueueCachedData({ items: [], currentIndex: 0, queueId: 'unknown' }, USER_A);
		expect(getQueueCachedData(USER_A)).toBeNull();
	});
});
