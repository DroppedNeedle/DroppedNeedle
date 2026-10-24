import { beforeEach, describe, expect, it, vi } from 'vitest';

const { apiMock, statusMock, cacheMock, cacheTtls, authMock } = vi.hoisted(() => {
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
			reset: vi.fn(),
			markConsumed: vi.fn()
		},
		cacheMock: {
			getQueueCachedData: vi.fn().mockReturnValue(null),
			setQueueCachedData: vi.fn(),
			removeQueueCachedData: vi.fn()
		},
		cacheTtls: { discoverQueueAutoGenerate: true, discoverQueuePollingInterval: 1000 },
		authMock: { user: { id: 'user-1' } }
	};
});

vi.mock('$lib/api/client', () => ({ api: apiMock }));
vi.mock('$lib/stores/discoverQueueStatus', () => ({ discoverQueueStatusStore: statusMock }));
vi.mock('$lib/utils/discoverQueueCache', () => cacheMock);
vi.mock('$lib/stores/cacheTtl.svelte', () => ({ getCacheTTLs: () => cacheTtls }));
vi.mock('$lib/stores/authStore.svelte', () => ({
	authStore: authMock
}));
vi.mock('$lib/queries/QueryClient', () => ({
	invalidateQueriesWithPersister: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/queries/discover/DiscoverDemand.svelte', () => ({
	recordDiscoverActivity: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/queries/musicbrainz/sourceScope.svelte', () => ({
	musicBrainzSourceKey: () => ({ source_mode: 'brainzmash', source_id: 'source-a', generation: 1 }),
	subscribeMusicBrainzSourceScope: () => () => {},
	watchMusicBrainzSourceScope: () => () => {}
}));

import { invalidateQueriesWithPersister } from '$lib/queries/QueryClient';
import { DiscoverQueryKeyFactory } from '$lib/queries/discover/DiscoverQueryKeyFactory';
import { discoverQueueDeck } from './discoverQueueDeck.svelte';

// Promise.withResolvers needs Node 22+; this repo runs Node 20. The deferred
// below keeps the same linear hand-off for the late-response tests.
function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((settle) => {
		resolve = settle;
	});
	return { promise, resolve };
}

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
		cacheTtls.discoverQueueAutoGenerate = true;
		statusMock.triggerGenerate.mockReset().mockResolvedValue(undefined);
		authMock.user.id = 'user-1';
		statusMock.listeners.length = 0;
		cacheMock.getQueueCachedData.mockReturnValue(null);
		discoverQueueDeck.destroy();
	});

	it('settles a building-to-idle queue without another generation or inline build', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'building' });
		await discoverQueueDeck.init();
		expect(discoverQueueDeck.phase).toBe('building');

		statusMock.emit({ status: 'idle' });

		expect(discoverQueueDeck.phase).toBe('waiting');
		expect(discoverQueueDeck.replacing).toBe(false);
		expect(statusMock.stopPolling).toHaveBeenCalledOnce();
		expect(statusMock.listeners).toHaveLength(0);
		expect(statusMock.triggerGenerate).not.toHaveBeenCalled();
		expect(apiMock.global.get).not.toHaveBeenCalled();
	});

	it('settles an immediate idle generation response and keeps automatic force policy', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'idle' });
		statusMock.triggerGenerate.mockImplementation(async () => {
			statusMock.emit({ status: 'building' });
			statusMock.emit({ status: 'idle' });
		});

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.phase).toBe('waiting');
		expect(statusMock.triggerGenerate).toHaveBeenCalledExactlyOnceWith(false);
		expect(statusMock.stopPolling).toHaveBeenCalledOnce();
		expect(apiMock.global.get).not.toHaveBeenCalled();
	});

	it('keeps existing cards when a forced replacement returns idle', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });
		await discoverQueueDeck.init();
		discoverQueueDeck.buildNow();
		statusMock.emit({ status: 'idle' });

		expect(discoverQueueDeck.phase).toBe('ready');
		expect(discoverQueueDeck.current?.release_group_mbid).toBe('rg-1');
		expect(discoverQueueDeck.replacing).toBe(false);
		expect(statusMock.triggerGenerate).toHaveBeenCalledExactlyOnceWith(true);
		expect(statusMock.stopPolling).toHaveBeenCalledOnce();
		expect(apiMock.global.get).not.toHaveBeenCalled();
	});

	it('finishes without generation when automatic generation is disabled', async () => {
		cacheTtls.discoverQueueAutoGenerate = false;
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });
		await discoverQueueDeck.init();
		discoverQueueDeck.finish();

		expect(discoverQueueDeck.phase).toBe('finished');
		expect(statusMock.triggerGenerate).not.toHaveBeenCalled();
	});

	it('dedupes duplicate and missing release group IDs from a fetched queue', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'ready' });
		apiMock.global.get.mockResolvedValue({
			items: [makeItem('rg-1'), makeItem(''), makeItem('rg-1'), makeItem('rg-2')],
			queue_id: 'q-fetched'
		});

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual(['rg-1', 'rg-2']);
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
	it.each([
		['negative', -1, 0],
		['oversized', 99, 1]
	] as const)(
		'dedupes cached items and clamps a %s index after dedupe',
		async (_label, currentIndex, expectedIndex) => {
			cacheMock.getQueueCachedData.mockReturnValue({
				data: {
					items: [makeItem('rg-1'), makeItem(''), makeItem('rg-1'), makeItem('rg-2')],
					currentIndex,
					queueId: 'q-cached'
				},
				timestamp: Date.now()
			});
			apiMock.global.post.mockResolvedValue({ in_library: [] });

			await discoverQueueDeck.init();

			expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual(['rg-1', 'rg-2']);
			expect(discoverQueueDeck.currentIndex).toBe(expectedIndex);
			expect(apiMock.global.post).toHaveBeenCalledWith(
				expect.stringContaining('validate'),
				{ release_group_mbids: ['rg-1', 'rg-2'] },
				expect.anything()
			);
		}
	);
	it('resumes the cached current item after deduping earlier duplicates', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: {
				items: [
					makeItem('rg-dup'),
					makeItem('rg-dup'),
					makeItem('rg-current'),
					makeItem('rg-later')
				],
				currentIndex: 2,
				queueId: 'q-cached'
			},
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual([
			'rg-dup',
			'rg-current',
			'rg-later'
		]);
		expect(discoverQueueDeck.currentIndex).toBe(1);
		expect(discoverQueueDeck.current?.release_group_mbid).toBe('rg-current');
	});

	it('clamps the original index when the cached current item has no release group ID', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: {
				items: [makeItem('rg-dup'), makeItem('rg-dup'), makeItem(''), makeItem('rg-later')],
				currentIndex: 2,
				queueId: 'q-cached'
			},
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual([
			'rg-dup',
			'rg-later'
		]);
		expect(discoverQueueDeck.currentIndex).toBe(1);
		expect(discoverQueueDeck.current?.release_group_mbid).toBe('rg-later');
	});

	it('validation drops items that entered the library', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: ['rg-1'] });

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.release_group_mbid)).toEqual(['rg-2']);
		expect(cacheMock.setQueueCachedData).toHaveBeenCalledWith(
			expect.objectContaining({ queueId: 'q1' }),
			'user-1'
		);
	});
	it('restores the cached queue ID instead of persisting a prior account identifier', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'ready' });
		apiMock.global.get.mockResolvedValue({
			items: [makeItem('rg-prior')],
			queue_id: 'q-user-1'
		});

		await discoverQueueDeck.init();

		authMock.user.id = 'user-2';
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 0, queueId: 'q-user-2' },
			timestamp: Date.now()
		});
		apiMock.global.post.mockResolvedValue({ in_library: ['rg-1'] });

		await discoverQueueDeck.init();

		expect(cacheMock.setQueueCachedData).toHaveBeenNthCalledWith(
			1,
			expect.objectContaining({ queueId: 'q-user-1' }),
			'user-1'
		);
		expect(cacheMock.setQueueCachedData).toHaveBeenLastCalledWith(
			expect.objectContaining({ queueId: 'q-user-2' }),
			'user-2'
		);
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
			queryKey: DiscoverQueryKeyFactory.discover('user-1')
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
	it('keeps the loaded deck usable while an explicit forced replacement builds or fails', async () => {
		cacheMock.getQueueCachedData.mockReturnValue({
			data: { items: [makeItem('rg-1'), makeItem('rg-2')], currentIndex: 0, queueId: 'q1' },
			timestamp: Date.now()
		});
		await discoverQueueDeck.init();
		discoverQueueDeck.buildNow();
		expect(statusMock.triggerGenerate).toHaveBeenCalledWith(true);
		expect(discoverQueueDeck.phase).toBe('ready');
		discoverQueueDeck.next();
		expect(discoverQueueDeck.current?.release_group_mbid).toBe('rg-2');
		statusMock.emit({ status: 'error', error: 'replacement failed' });
		expect(discoverQueueDeck.current?.release_group_mbid).toBe('rg-2');
		expect(discoverQueueDeck.replacing).toBe(false);
	});

	it('publishes candidates without awaiting hydration and hydrates only current and next', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'ready' });
		apiMock.global.get.mockImplementation((url: string) =>
			url.endsWith('/queue')
				? Promise.resolve({
						items: ['a', 'b', 'c', 'd'].map((id) => ({ ...makeItem(id), enrichment: null })),
						queue_id: 'light'
					})
				: new Promise<unknown>(() => {})
		);
		await discoverQueueDeck.init();
		expect(discoverQueueDeck.phase).toBe('ready');
		expect(discoverQueueDeck.current?.release_group_mbid).toBe('a');
		expect(apiMock.global.get.mock.calls.map(([url]) => url)).toEqual([
			'/api/v1/discover/queue',
			'/api/v1/discover/queue/enrich/a',
			'/api/v1/discover/queue/enrich/b'
		]);
		discoverQueueDeck.next();
		expect(apiMock.global.get).toHaveBeenLastCalledWith(
			'/api/v1/discover/queue/enrich/c',
			expect.anything()
		);
	});

	it('ignores a late queue response from a destroyed session', async () => {
		statusMock.fetchStatus.mockResolvedValue({ status: 'ready' });
		const response = deferred<unknown>();
		apiMock.global.get.mockReturnValue(response.promise);
		const pending = discoverQueueDeck.init();
		await vi.waitFor(() => expect(apiMock.global.get).toHaveBeenCalled());
		discoverQueueDeck.destroy();
		response.resolve({ items: [makeItem('stale')], queue_id: 'stale' });
		await pending;
		expect(discoverQueueDeck.phase).toBe('idle');
		expect(discoverQueueDeck.queue).toEqual([]);
	});
});
