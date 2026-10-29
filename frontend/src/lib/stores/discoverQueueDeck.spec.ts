import { beforeEach, describe, expect, it, vi } from 'vitest';

const { queries, statusMock, settings, authMock } = vi.hoisted(() => {
	type Listener = (s: { status: string; error?: string }) => void;
	const listeners: Listener[] = [];
	return {
		// The deck hears only statuses after it starts watching.
		statusMock: {
			listeners,
			emit(state: { status: string; error?: string }) {
				for (const l of [...listeners]) l(state);
			}
		},
		queries: {
			fetchDiscoverQueue: vi.fn(),
			fetchDiscoverQueueStatus: vi.fn(),
			fetchQueueEnrichment: vi.fn().mockResolvedValue(null),
			fetchQueueInLibrary: vi.fn().mockResolvedValue({ in_library: [] }),
			forgetSavedDeck: vi.fn().mockResolvedValue(undefined),
			generateDiscoverQueue: vi.fn().mockResolvedValue({ status: 'building' }),
			ignoreQueueItem: vi.fn().mockResolvedValue({}),
			markDiscoverQueueConsumed: vi.fn().mockResolvedValue(undefined),
			peekSavedDeck: vi.fn().mockReturnValue(null),
			readSavedDeck: vi.fn().mockResolvedValue(null),
			saveDeck: vi.fn().mockResolvedValue(undefined),
			watchDiscoverQueueStatus: vi.fn((l: Listener) => {
				listeners.push(l);
				return () => {
					const i = listeners.indexOf(l);
					if (i >= 0) listeners.splice(i, 1);
				};
			})
		},
		settings: { autoGenerate: true },
		authMock: { user: { id: 'user-1' } }
	};
});

vi.mock('$lib/queries/discover/DiscoverQueueQueries', () => queries);
vi.mock('$lib/queries/AdvancedSettingsReads.svelte', () => ({
	discoverQueueAutoGenerate: async () => settings.autoGenerate
}));
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

function savedDeck(items: ReturnType<typeof makeItem>[], currentIndex: number, queueId: string) {
	return { items, currentIndex, queueId, savedAt: Date.now() };
}

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
		musicbrainz_id: mbid,
		title: `Album ${mbid}`,
		artist_name: 'Artist',
		artist_id: 'a-1',
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
		authMock.user.id = 'user-1';
		statusMock.listeners.length = 0;
		queries.readSavedDeck.mockResolvedValue(null);
		queries.fetchQueueInLibrary.mockResolvedValue({ in_library: [] });
		queries.generateDiscoverQueue.mockResolvedValue({ status: 'building' });
		settings.autoGenerate = true;
		discoverQueueDeck.destroy();
	});

	it('dedupes duplicate and missing release group IDs from a fetched queue', async () => {
		queries.fetchDiscoverQueueStatus.mockResolvedValue({ status: 'ready' });
		queries.fetchDiscoverQueue.mockResolvedValue({
			items: [makeItem('rg-1'), makeItem(''), makeItem('rg-1'), makeItem('rg-2')],
			queue_id: 'q-fetched'
		});

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.musicbrainz_id)).toEqual(['rg-1', 'rg-2']);
	});

	it.each([
		['negative', -1, 0],
		['oversized', 99, 1]
	] as const)(
		'dedupes cached items and clamps a %s index after dedupe',
		async (_label, currentIndex, expectedIndex) => {
			queries.readSavedDeck.mockResolvedValue(
				savedDeck(
					[makeItem('rg-1'), makeItem(''), makeItem('rg-1'), makeItem('rg-2')],
					currentIndex,
					'q-cached'
				)
			);
			queries.fetchQueueInLibrary.mockResolvedValue({ in_library: [] });

			await discoverQueueDeck.init();

			expect(discoverQueueDeck.queue.map((i) => i.musicbrainz_id)).toEqual(['rg-1', 'rg-2']);
			expect(discoverQueueDeck.currentIndex).toBe(expectedIndex);
			expect(queries.fetchQueueInLibrary).toHaveBeenCalledWith(['rg-1', 'rg-2']);
		}
	);

	it('validation drops items that entered the library', async () => {
		queries.readSavedDeck.mockResolvedValue(
			savedDeck([makeItem('rg-1'), makeItem('rg-2')], 0, 'q1')
		);
		queries.fetchQueueInLibrary.mockResolvedValue({ in_library: ['rg-1'] });

		await discoverQueueDeck.init();

		expect(discoverQueueDeck.queue.map((i) => i.musicbrainz_id)).toEqual(['rg-2']);
		expect(queries.saveDeck).toHaveBeenCalledWith(expect.objectContaining({ queueId: 'q1' }));
	});
	it('restores the cached queue ID instead of persisting a prior account identifier', async () => {
		queries.fetchDiscoverQueueStatus.mockResolvedValue({ status: 'ready' });
		queries.fetchDiscoverQueue.mockResolvedValue({
			items: [makeItem('rg-prior')],
			queue_id: 'q-user-1'
		});

		await discoverQueueDeck.init();

		authMock.user.id = 'user-2';
		queries.readSavedDeck.mockResolvedValue(
			savedDeck([makeItem('rg-1'), makeItem('rg-2')], 0, 'q-user-2')
		);
		queries.fetchQueueInLibrary.mockResolvedValue({ in_library: ['rg-1'] });

		await discoverQueueDeck.init();

		expect(queries.saveDeck).toHaveBeenNthCalledWith(
			1,
			expect.objectContaining({ queueId: 'q-user-1' })
		);
		expect(queries.saveDeck).toHaveBeenLastCalledWith(
			expect.objectContaining({ queueId: 'q-user-2' })
		);
	});

	it('keeps the loaded deck usable while an explicit forced replacement builds or fails', async () => {
		queries.readSavedDeck.mockResolvedValue(
			savedDeck([makeItem('rg-1'), makeItem('rg-2')], 0, 'q1')
		);
		await discoverQueueDeck.init();
		discoverQueueDeck.buildNow();
		expect(queries.generateDiscoverQueue).toHaveBeenCalledWith(true);
		expect(discoverQueueDeck.phase).toBe('ready');
		discoverQueueDeck.next();
		expect(discoverQueueDeck.current?.musicbrainz_id).toBe('rg-2');
		statusMock.emit({ status: 'error', error: 'replacement failed' });
		expect(discoverQueueDeck.current?.musicbrainz_id).toBe('rg-2');
		expect(discoverQueueDeck.replacing).toBe(false);
	});

	it('ignores a late queue response from a destroyed session', async () => {
		queries.fetchDiscoverQueueStatus.mockResolvedValue({ status: 'ready' });
		const response = deferred<unknown>();
		queries.fetchDiscoverQueue.mockReturnValue(response.promise);
		const pending = discoverQueueDeck.init();
		await vi.waitFor(() => expect(queries.fetchDiscoverQueue).toHaveBeenCalled());
		discoverQueueDeck.destroy();
		response.resolve({ items: [makeItem('stale')], queue_id: 'stale' });
		await pending;
		expect(discoverQueueDeck.phase).toBe('idle');
		expect(discoverQueueDeck.queue).toEqual([]);
	});
});
