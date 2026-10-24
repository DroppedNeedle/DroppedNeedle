/** Real deck + real status store: only HTTP, session/source and cache edges are mocked. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const { apiMock, cacheMock } = vi.hoisted(() => ({
	apiMock: { global: { get: vi.fn(), post: vi.fn() } },
	cacheMock: {
		getQueueCachedData: vi.fn(),
		setQueueCachedData: vi.fn(),
		removeQueueCachedData: vi.fn()
	}
}));
vi.mock('$app/environment', () => ({ browser: true }));
vi.mock('$lib/api/client', () => ({ api: apiMock, ApiError: class extends Error {} }));
vi.mock('$lib/utils/discoverQueueCache', () => cacheMock);
vi.mock('$lib/stores/cacheTtl.svelte', () => ({
	getCacheTTLs: () => ({ discoverQueueAutoGenerate: true, discoverQueuePollingInterval: 1000 })
}));
vi.mock('$lib/stores/authStore.svelte', () => ({ authStore: { user: { id: 'u1' } } }));
vi.mock('$lib/queries/QueryClient', () => ({ invalidateQueriesWithPersister: vi.fn() }));
vi.mock('$lib/queries/discover/DiscoverDemand.svelte', () => ({
	recordDiscoverActivity: vi.fn().mockResolvedValue(undefined)
}));
vi.mock('$lib/queries/musicbrainz/sourceScope.svelte', () => ({
	musicBrainzSourceKey: () => 'source-a',
	subscribeMusicBrainzSourceScope: () => () => {},
	watchMusicBrainzSourceScope: () => () => {}
}));
import { discoverQueueDeck as deck } from './discoverQueueDeck.svelte';
import { discoverQueueStatusStore as status } from './discoverQueueStatus';

function deferred<T>() {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((settle) => {
		resolve = settle;
	});
	return { promise, resolve };
}
function item(id: string) {
	return { release_group_mbid: id, album_name: id, artist_name: 'Artist', enrichment: {} };
}
async function turns() {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}
beforeEach(() => {
	vi.clearAllMocks();
	cacheMock.getQueueCachedData.mockReturnValue(null);
	deck.destroy();
});
afterEach(() => deck.destroy());

it.each([false, true])(
	'adopts a new queue despite a pre-generation idle GET (existing cards: %s)',
	async (cached) => {
		const old = deferred<unknown>();
		const generation = deferred<unknown>();
		if (cached)
			cacheMock.getQueueCachedData.mockReturnValue({
				data: { items: [item('old-rg')], currentIndex: 0, queueId: 'old' },
				timestamp: Date.now()
			});
		let calls = 0;
		apiMock.global.get.mockImplementation((url: string) => {
			if (url.endsWith('/queue/status')) {
				calls++;
				if (!cached && calls === 1) return Promise.resolve({ status: 'building' });
				if (calls === (cached ? 1 : 2)) return old.promise;
				return Promise.resolve({ status: 'ready', queue_id: 'new', item_count: 1 });
			}
			return Promise.resolve({ items: [item('new-rg')], queue_id: 'new' });
		});
		apiMock.global.post.mockResolvedValue({ in_library: [] });
		await deck.init();
		const oldPoll = status.fetchStatus();
		apiMock.global.post.mockReturnValue(generation.promise);
		deck.buildNow();
		old.resolve({ status: 'idle' });
		await oldPoll;
		expect(deck.phase).toBe(cached ? 'ready' : 'building');
		expect(deck.replacing).toBe(true);
		generation.resolve({ status: 'building' });
		await turns();
		expect(status.isPolling).toBe(true);
		await status.fetchStatus();
		await turns();
		expect(deck.phase).toBe('ready');
		expect(deck.current?.release_group_mbid).toBe('new-rg');
		expect(
			apiMock.global.post.mock.calls.filter(([url]) => url.endsWith('/queue/generate'))
		).toEqual([['/api/v1/discover/queue/generate', { force: true }]]);
	}
);

it('settles a genuine idle generation POST without an inline queue GET or second POST', async () => {
	apiMock.global.get.mockResolvedValue({ status: 'idle' });
	apiMock.global.post.mockResolvedValue({ status: 'idle' });
	await deck.init();
	expect(deck.phase).toBe('waiting');
	expect(status.isPolling).toBe(false);
	expect(apiMock.global.post).toHaveBeenCalledExactlyOnceWith('/api/v1/discover/queue/generate', {
		force: false
	});
	expect(apiMock.global.get).toHaveBeenCalledExactlyOnceWith('/api/v1/discover/queue/status');
});

it('settles a genuine building-to-idle status GET without generation', async () => {
	apiMock.global.get
		.mockResolvedValueOnce({ status: 'building' })
		.mockResolvedValueOnce({ status: 'idle' });
	await deck.init();
	await status.fetchStatus();
	expect(deck.phase).toBe('waiting');
	expect(status.isPolling).toBe(false);
	expect(apiMock.global.post).not.toHaveBeenCalled();
	expect(apiMock.global.get).toHaveBeenCalledTimes(2);
});
