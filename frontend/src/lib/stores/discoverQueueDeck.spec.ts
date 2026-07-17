import { beforeEach, describe, expect, it, vi } from 'vitest';

const { apiMock, statusMock, cacheMock, cacheTtls } = vi.hoisted(() => {
	type StatusState = { status: string; error?: string };
	type Listener = (s: StatusState) => void;
	const listeners: Listener[] = [];
	// mirror Svelte's writable contract: subscribe fires synchronously with the
	// current value, and that initial value is what the deck's watcher must ignore
	let current: StatusState = { status: 'unknown' };
	return {
		apiMock: {
			global: {
				get: vi.fn(),
				post: vi.fn().mockResolvedValue({})
			}
		},
		statusMock: {
			listeners,
			emit(state: StatusState) {
				current = state;
				for (const l of [...listeners]) l(state);
			},
			subscribe: vi.fn((l: Listener) => {
				listeners.push(l);
				l(current);
				return () => {
					const i = listeners.indexOf(l);
					if (i >= 0) listeners.splice(i, 1);
				};
			}),
			fetchStatus: vi.fn(),
			triggerGenerate: vi.fn().mockResolvedValue(undefined),
			startPolling: vi.fn(),
			stopPolling: vi.fn(),
			markConsumed: vi.fn()
		},
		cacheMock: {
			getQueueCachedData: vi.fn().mockReturnValue(null),
			setQueueCachedData: vi.fn(),
			removeQueueCachedData: vi.fn()
		},
		cacheTtls: { discoverQueueAutoGenerate: true, discoverQueuePollingInterval: 1000 }
	};
});

vi.mock('$lib/api/client', () => ({ api: apiMock }));
vi.mock('$lib/stores/discoverQueueStatus', () => ({ discoverQueueStatusStore: statusMock }));
vi.mock('$lib/utils/discoverQueueCache', () => cacheMock);
vi.mock('$lib/stores/cacheTtl', () => ({ getCacheTTLs: () => cacheTtls }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: { user: { id: 'user-1' } }
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));

import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { discoverQueueDeck } from './discoverQueueDeck.svelte';

function makeItem(mbid: string) {
	return {
		release_group_mbid: mbid,
		album_name: `Album ${mbid}`,
		artist_name: 'Artist',
		artist_mbid: 'a-1',
		cover_url: null,
		recommendation_reason: 'reason',
		is_wildcard: false,
		in_library: false,
		enrichment: {
			artist_mbid: 'a-1',
			release_date: null,
			country: null,
			tags: [],
			youtube_url: null,
			youtube_search_url: '',
			youtube_search_available: false,
			artist_description: null,
			listen_count: null
		}
	};
}

describe('discoverQueueDeck state machine', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		statusMock.listeners.length = 0;
		cacheMock.getQueueCachedData.mockReturnValue(null);
		discoverQueueDeck.destroy();
	});

	it('resumes instantly from a cached queue and validates it', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 1, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.phase).toBe('ready');
		expect(discoverQueueDeck.currentIndex).toBe(1);
		expect(discoverQueueDeck.queue).toHaveLength(2);
		// validated against the library
		expect(apiMock.global.post).toHaveBeenCalledWith(
			expect.stringContaining('validate'),
			{ release_group_mbids: ['rg-1', 'rg-2'] },
			expect.anything()
		);
	});

	it('validation drops items that entered the library', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: ['rg-1'] });

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual(['rg-2']);
	});

	it('removeByMbid reconciles a queue that has not been loaded yet', () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});

		discoverQueueDeck.removeByMbid('rg-1');

		expect(cacheMock.setQueueCachedData).toHaveBeenCalledWith(
			expect.objectContaining({ items: [makeItem('rg-2')] }),
			'user-1'
		);
	});

	it('consumes a ready background build when there is no cache', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'ready' });
		apiMock.global.get.mockResolvedValue({
			items: [makeItem('rg-9')],
			queue_id: 'q-fresh'
		});

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.phase).toBe('ready');
		expect(discoverQueueDeck.queue).toHaveLength(1);
		expect(statusMock.markConsumed).toHaveBeenCalled();
		expect(cacheMock.setQueueCachedData).toHaveBeenCalled();
	});

	it('waits in building phase and adopts the queue when the build finishes', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'building' });
		apiMock.global.get.mockResolvedValue({
			items: [makeItem('rg-5')],
			queue_id: 'q-built'
		});

		await discoverQueueDeck.init();
		expect(discoverQueueDeck.phase).toBe('building');

		statusMock.emit({ status: 'ready' });
		await vi.waitFor(() => {
			expect(discoverQueueDeck.phase).toBe('ready');
		});
		expect(discoverQueueDeck.queue[0].release_group_mbid).toBe('rg-5');
	});

	it('surfaces a failed background build with its error', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'building' });

		await discoverQueueDeck.init();
		statusMock.emit({ status: 'error', error: 'boom' });

		expect(discoverQueueDeck.phase).toBe('error');
		expect(discoverQueueDeck.errorMessage).toBe('boom');
	});

	it('ignoreCurrent removes the item and persists', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });
		await discoverQueueDeck.init();
		apiMock.global.post.mockClear();

		await discoverQueueDeck.ignoreCurrent();

		expect(apiMock.global.post).toHaveBeenCalledWith(
			expect.stringContaining('ignore'),
			expect.objectContaining({ release_group_mbid: 'rg-1' }),
			expect.anything()
		);
		expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual(['rg-2']);
		expect(invalidateQueriesWithPersister).toHaveBeenCalledWith({
			queryKey: ['discover', 'user-1']
		});
	});

	it('finish clears the cache and brews a fresh queue', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });
		await discoverQueueDeck.init();

		discoverQueueDeck.finish();

		expect(discoverQueueDeck.phase).toBe('finished');
		expect(cacheMock.removeQueueCachedData).toHaveBeenCalledWith('user-1');
		expect(statusMock.triggerGenerate).toHaveBeenCalledWith(false);
	});
});
